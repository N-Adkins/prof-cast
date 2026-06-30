//! Thread enumeration and per-thread stack capture for the Windows backend.
//!
//! A sample is taken by suspending a thread, reading its register context, and
//! walking the stack with `StackWalk64` (which consults the `DbgHelp` function
//! tables set up by [`Symbolizer`](super::symbolize::Symbolizer)). The thread is
//! resumed immediately afterwards. Safety here rests on the documented Win32
//! contracts for these calls, so the granular unsafe-hygiene lints are relaxed
//! for the module - the interesting invariants are the ABI ones noted inline.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::cast_ptr_alignment,
    clippy::multiple_unsafe_ops_per_block,
    clippy::undocumented_unsafe_blocks
)]

use std::collections::HashMap;
use std::mem;
use std::ptr;

use profcast_core::{ProfcastError, Result};

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Diagnostics::Debug::{
    ADDRESS64, CONTEXT, GetThreadContext, STACKFRAME64, StackWalk64, SymFunctionTableAccess64,
    SymGetModuleBase64,
};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, GetCurrentProcessId, GetCurrentThreadId, OpenProcess, OpenThread,
    ResumeThread, SuspendThread,
};
use windows_sys::Win32::System::WindowsProgramming::QueryThreadCycleTime;

/// A raw Win32 process handle (alias kept local so callers need not name the
/// `windows-sys` type directly).
pub(super) type Handle = HANDLE;

// Access rights, declared locally to avoid depending on which `windows-sys`
// feature happens to export each constant.
const PROCESS_QUERY_INFORMATION: u32 = 0x0400;
const PROCESS_VM_READ: u32 = 0x0010;
const SYNCHRONIZE: u32 = 0x0010_0000;
const THREAD_GET_CONTEXT: u32 = 0x0008;
const THREAD_SUSPEND_RESUME: u32 = 0x0002;
const THREAD_QUERY_INFORMATION: u32 = 0x0040;

/// `STACKFRAME64.AddrMode` value for a flat (non-segmented) address.
const ADDR_MODE_FLAT: i32 = 3;
/// `MachineType` argument to `StackWalk64` for x86-64.
const IMAGE_FILE_MACHINE_AMD64: u32 = 0x8664;

// `CONTEXT.ContextFlags` bits for x86-64: the control and integer registers are
// all `StackWalk64` needs (RIP/RSP/RBP).
const CONTEXT_AMD64: u32 = 0x0010_0000;
const CONTEXT_CONTROL: u32 = CONTEXT_AMD64 | 0x0000_0001;
const CONTEXT_INTEGER: u32 = CONTEXT_AMD64 | 0x0000_0002;

/// Hard cap on walked frames, guarding against a corrupt or cyclic stack.
const MAX_FRAMES: usize = 256;

/// `CONTEXT` forced to the 16-byte alignment `GetThreadContext` requires on
/// x86-64. The `windows-sys` `CONTEXT` is only `#[repr(C)]` (8-byte aligned via
/// its `u64` fields), and an under-aligned buffer makes `GetThreadContext` fail.
#[repr(C, align(16))]
struct AlignedContext(CONTEXT);

/// Returns the calling process's id.
pub(super) fn current_pid() -> u32 {
    // SAFETY: no preconditions.
    unsafe { GetCurrentProcessId() }
}

/// A process handle to sample, tracking whether we must close it on drop. The
/// `Current` pseudo-handle and a handle borrowed from [`Launched`] are not
/// owned; one opened from a pid is.
///
/// [`Launched`]: super::launch::Launched
pub(super) struct ProcessHandle {
    handle: HANDLE,
    owned: bool,
}

impl ProcessHandle {
    /// The pseudo-handle for the current process (never closed).
    pub(super) fn current() -> Self {
        // SAFETY: returns a constant pseudo-handle; cannot fail.
        let handle = unsafe { GetCurrentProcess() };
        Self {
            handle,
            owned: false,
        }
    }

    /// Wraps a handle owned elsewhere (e.g. a launched child); not closed here.
    pub(super) fn borrowed(handle: HANDLE) -> Self {
        Self {
            handle,
            owned: false,
        }
    }

    /// Opens an existing process by id with the rights `DbgHelp`'s reads and the
    /// stack walk require.
    ///
    /// # Errors
    ///
    /// Returns [`ProfcastError::Capture`] if the process cannot be opened
    /// (gone, or access denied).
    pub(super) fn open(pid: u32) -> Result<Self> {
        // SAFETY: a straightforward call with a valid desired-access mask.
        let handle = unsafe {
            OpenProcess(
                PROCESS_QUERY_INFORMATION | PROCESS_VM_READ | SYNCHRONIZE,
                0,
                pid,
            )
        };
        if handle.is_null() {
            return Err(ProfcastError::Capture(format!(
                "cannot open process {pid}: {}",
                std::io::Error::last_os_error()
            )));
        }
        Ok(Self {
            handle,
            owned: true,
        })
    }

    /// The raw handle, for passing to the Win32 sampling calls.
    pub(super) fn raw(&self) -> HANDLE {
        self.handle
    }

    /// Whether the process has terminated.
    pub(super) fn has_exited(&self) -> bool {
        super::launch::process_has_exited(self.handle)
    }
}

impl Drop for ProcessHandle {
    fn drop(&mut self) {
        if self.owned && !self.handle.is_null() {
            // SAFETY: `handle` is one we opened and have not closed.
            unsafe {
                CloseHandle(self.handle);
            }
        }
    }
}

/// Lists the thread ids belonging to `pid`, excluding the calling thread (so a
/// self-profile never suspends its own sampler). Returns an empty vector if the
/// snapshot cannot be taken; the caller treats a barren tick as "no samples".
pub(super) fn thread_ids(pid: u32) -> Vec<u32> {
    // SAFETY: snapshotting all threads; `0` is the ignored process argument.
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        tracing::debug!(
            pid,
            error = %std::io::Error::last_os_error(),
            "thread snapshot failed; skipping this tick",
        );
        return Vec::new();
    }

    // Thread ids are unique system-wide, so excluding our own by id is enough.
    let self_tid = unsafe { GetCurrentThreadId() };
    let mut entry: THREADENTRY32 = unsafe { mem::zeroed() };
    entry.dwSize = mem::size_of::<THREADENTRY32>() as u32;

    let mut tids = Vec::new();
    // SAFETY: `entry` is sized; `snapshot` is the handle just created.
    let mut ok = unsafe { Thread32First(snapshot, ptr::from_mut(&mut entry)) };
    while ok != 0 {
        if entry.th32OwnerProcessID == pid && entry.th32ThreadID != self_tid {
            tids.push(entry.th32ThreadID);
        }
        ok = unsafe { Thread32Next(snapshot, ptr::from_mut(&mut entry)) };
    }

    // SAFETY: closing the snapshot handle we opened.
    unsafe {
        CloseHandle(snapshot);
    }
    tids
}

/// Reads the current CPU cycle count of every thread of `pid`, for seeding the
/// sampler's per-thread baselines before the drain loop starts.
///
/// For a launched child this is called while it is still suspended, so its
/// threads read ~0 cycles and any work after the resume registers as advancement
/// on the very first tick; for an already-running target it simply captures a
/// baseline so that first tick is not wasted establishing one. Threads that
/// cannot be opened or queried are omitted — they just fall back to the normal
/// first-sighting path in the loop.
pub(super) fn snapshot_thread_cycles(pid: u32) -> HashMap<u32, u64> {
    let mut cycles = HashMap::new();
    for tid in thread_ids(pid) {
        // SAFETY: opening one of the target's threads for a cycle-time query only.
        let thread = unsafe { OpenThread(THREAD_QUERY_INFORMATION, 0, tid) };
        if thread.is_null() {
            continue;
        }
        if let Some(count) = thread_cycle_time(thread) {
            cycles.insert(tid, count);
        }
        // SAFETY: `thread` is the handle we just opened.
        unsafe {
            CloseHandle(thread);
        }
    }
    cycles
}

/// Samples thread `tid` only if it actually ran since the previous tick.
///
/// `last_cycles` is the thread's CPU cycle count at the prior tick (`None` if it
/// has not been seen yet). The thread is walked into `out` (leaf-first) only when
/// its cycle count has advanced — i.e. it consumed CPU during the interval — so
/// idle threads parked in a wait do not accrue samples. The current cycle count
/// is returned for the caller to carry forward; `None` means the thread is gone.
pub(super) fn sample_running_thread(
    process: HANDLE,
    tid: u32,
    last_cycles: Option<u64>,
    out: &mut Vec<u64>,
) -> Option<u64> {
    // SAFETY: opening one of the target's threads with the sampling rights.
    let thread = unsafe {
        OpenThread(
            THREAD_GET_CONTEXT | THREAD_SUSPEND_RESUME | THREAD_QUERY_INFORMATION,
            0,
            tid,
        )
    };
    if thread.is_null() {
        // Threads come and go constantly; a thread that exited between the
        // snapshot and this open is expected, not an error.
        tracing::trace!(tid, "skipping thread that could not be opened");
        return None;
    }

    let cycles = thread_cycle_time(thread);
    // Walk only threads that advanced their CPU cycles since the last tick. With
    // no baseline yet (first sighting) we skip, so a thread parked in a wait the
    // whole run is never mistaken for on-CPU work on its first sample.
    let ran = matches!((last_cycles, cycles), (Some(prev), Some(cur)) if cur > prev);
    if ran {
        walk_open_thread(process, thread, tid, out);
    }

    // SAFETY: `thread` is the handle we just opened.
    unsafe {
        CloseHandle(thread);
    }
    cycles
}

/// Reads thread `thread`'s accumulated CPU cycle count, or `None` if the query
/// fails (e.g. the thread exited). Used to tell on-CPU threads from idle ones.
fn thread_cycle_time(thread: HANDLE) -> Option<u64> {
    let mut cycles: u64 = 0;
    // SAFETY: `thread` was opened with THREAD_QUERY_INFORMATION; `cycles` is a
    // valid out-param. A zero return signals failure.
    let ok = unsafe { QueryThreadCycleTime(thread, ptr::from_mut(&mut cycles)) };
    (ok != 0).then_some(cycles)
}

/// Inner half of [`walk_thread`], operating on an already-opened thread handle
/// so the suspend is always paired with a resume on every return path.
fn walk_open_thread(process: HANDLE, thread: HANDLE, tid: u32, out: &mut Vec<u64>) -> bool {
    // SAFETY: suspending a thread we hold a SUSPEND_RESUME handle to. A return
    // of (DWORD)-1 signals failure (e.g. the thread already exited).
    let prev = unsafe { SuspendThread(thread) };
    if prev == u32::MAX {
        tracing::trace!(tid, "could not suspend thread; skipping");
        return false;
    }

    let mut aligned: AlignedContext = unsafe { mem::zeroed() };
    aligned.0.ContextFlags = CONTEXT_CONTROL | CONTEXT_INTEGER;
    // SAFETY: `aligned.0` is a sized CONTEXT at the 16-byte alignment x86-64
    // requires (see `AlignedContext`); the thread is suspended.
    let got = unsafe { GetThreadContext(thread, ptr::from_mut(&mut aligned.0)) };
    if got != 0 {
        walk_stack(process, thread, &mut aligned.0, out);
    } else {
        tracing::debug!(
            tid,
            error = %std::io::Error::last_os_error(),
            "GetThreadContext failed; thread not sampled",
        );
    }

    // SAFETY: balance the suspend above. Best-effort: a failed resume cannot be
    // recovered here, and leaving the thread suspended is the worse option.
    unsafe {
        ResumeThread(thread);
    }
    !out.is_empty()
}

/// Runs the `StackWalk64` loop, pushing each frame's program counter into `out`.
fn walk_stack(process: HANDLE, thread: HANDLE, context: &mut CONTEXT, out: &mut Vec<u64>) {
    let mut frame: STACKFRAME64 = unsafe { mem::zeroed() };
    frame.AddrPC = flat_addr(context.Rip);
    frame.AddrFrame = flat_addr(context.Rbp);
    frame.AddrStack = flat_addr(context.Rsp);

    for _ in 0..MAX_FRAMES {
        // SAFETY: all pointers are to live, correctly-typed locals; the DbgHelp
        // function-table and module-base routines are the documented helpers,
        // and a null read/translate routine selects StackWalk's defaults
        // (ReadProcessMemory against `process`).
        let ok = unsafe {
            StackWalk64(
                IMAGE_FILE_MACHINE_AMD64,
                process,
                thread,
                ptr::from_mut(&mut frame),
                ptr::from_mut(context).cast(),
                None,
                Some(SymFunctionTableAccess64),
                Some(SymGetModuleBase64),
                None,
            )
        };
        if ok == 0 {
            break;
        }
        let pc = frame.AddrPC.Offset;
        if pc == 0 {
            break;
        }
        out.push(pc);
    }
}

/// Builds a flat-mode [`ADDRESS64`] at `offset`.
fn flat_addr(offset: u64) -> ADDRESS64 {
    ADDRESS64 {
        Offset: offset,
        Segment: 0,
        Mode: ADDR_MODE_FLAT,
    }
}
