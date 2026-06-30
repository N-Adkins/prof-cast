//! Launching a program suspended, so `DbgHelp` can be attached before it runs.
//!
//! `std::process::Command` gives no window between spawn and the child running;
//! `CreateProcessW` with `CREATE_SUSPENDED` does, mirroring the Linux backend's
//! pre-`exec` barrier. The child's primary thread stays suspended until
//! [`resume`](Launched::resume). The granular unsafe-hygiene lints are relaxed
//! here as in the sibling FFI modules.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::arithmetic_side_effects,
    clippy::multiple_unsafe_ops_per_block,
    clippy::undocumented_unsafe_blocks
)]

use std::mem;
use std::ptr;

use profcast_core::{ProfcastError, Result};

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
use windows_sys::Win32::System::Threading::{
    CreateProcessW, PROCESS_INFORMATION, ResumeThread, STARTUPINFOW, TerminateProcess,
    WaitForSingleObject,
};

/// `dwCreationFlags` bit holding the primary thread suspended at creation.
const CREATE_SUSPENDED: u32 = 0x0000_0004;
/// `WaitForSingleObject` non-blocking poll / signaled return.
const WAIT_OBJECT_0: u32 = 0;

/// A child process created suspended, plus the means to release, poll, and reap
/// it. Dropping it terminates the child if it is still running.
pub(super) struct Launched {
    pub(super) pid: u32,
    process: HANDLE,
    thread: HANDLE,
}

impl Launched {
    /// Creates the process in `argv` suspended (`argv[0]` is the program, looked
    /// up via the normal search path). The caller attaches `DbgHelp` and then
    /// calls [`resume`](Launched::resume).
    ///
    /// # Errors
    ///
    /// Returns [`ProfcastError::Capture`] if `argv` is empty or `CreateProcessW`
    /// fails (e.g. the program was not found).
    pub(super) fn spawn(argv: &[String]) -> Result<Self> {
        if argv.is_empty() {
            return Err(ProfcastError::Capture(
                "empty command: no program to launch".to_owned(),
            ));
        }

        // CreateProcessW takes a single, mutable command line; build it with
        // the standard Windows argument quoting, then NUL-terminate.
        let mut command_line: Vec<u16> = encode_command_line(argv);
        command_line.push(0);

        let mut startup: STARTUPINFOW = unsafe { mem::zeroed() };
        startup.cb = mem::size_of::<STARTUPINFOW>() as u32;
        let mut info: PROCESS_INFORMATION = unsafe { mem::zeroed() };

        // SAFETY: a null application name searches the path using the first
        // token of the (valid, NUL-terminated, writable) command line; the
        // startup/info structs are correctly sized.
        let ok = unsafe {
            CreateProcessW(
                ptr::null(),
                command_line.as_mut_ptr(),
                ptr::null(),
                ptr::null(),
                0,
                CREATE_SUSPENDED,
                ptr::null(),
                ptr::null(),
                ptr::from_ref(&startup),
                ptr::from_mut(&mut info),
            )
        };
        if ok == 0 {
            return Err(ProfcastError::Capture(format!(
                "could not launch '{}': {}",
                argv.first().map_or("", String::as_str),
                std::io::Error::last_os_error()
            )));
        }

        tracing::debug!(pid = info.dwProcessId, "launched suspended child");
        Ok(Self {
            pid: info.dwProcessId,
            process: info.hProcess,
            thread: info.hThread,
        })
    }

    /// The child's process handle, for sampling and symbolization.
    pub(super) fn process_handle(&self) -> HANDLE {
        self.process
    }

    /// Releases the child by resuming its primary thread.
    ///
    /// # Errors
    ///
    /// Returns [`ProfcastError::Capture`] if the resume fails.
    pub(super) fn resume(&mut self) -> Result<()> {
        // SAFETY: resuming the primary thread we hold a handle to. A return of
        // (DWORD)-1 indicates failure.
        let prev = unsafe { ResumeThread(self.thread) };
        if prev == u32::MAX {
            return Err(ProfcastError::Capture(format!(
                "could not resume launched process {}: {}",
                self.pid,
                std::io::Error::last_os_error()
            )));
        }
        tracing::debug!(pid = self.pid, "resumed launched child");
        Ok(())
    }

    /// Returns whether the child has exited.
    pub(super) fn has_exited(&self) -> bool {
        process_has_exited(self.process)
    }
}

impl Drop for Launched {
    fn drop(&mut self) {
        // Never leave the child running: if we never resumed it, or it is still
        // alive, terminate it. A resumed child that has already exited is left
        // alone; only its handles are closed.
        if !self.has_exited() {
            // SAFETY: terminating the child we created.
            unsafe {
                TerminateProcess(self.process, 1);
            }
        }
        // SAFETY: closing the two handles CreateProcessW handed us.
        unsafe {
            CloseHandle(self.thread);
            CloseHandle(self.process);
        }
    }
}

/// Whether `process` has terminated, via a non-blocking wait on its handle.
pub(super) fn process_has_exited(process: HANDLE) -> bool {
    // SAFETY: a zero-timeout wait on a valid process handle.
    let rc = unsafe { WaitForSingleObject(process, 0) };
    rc == WAIT_OBJECT_0
}

/// Joins `argv` into a single command line using the standard Windows quoting
/// rules (see "Everyone quotes command line arguments the wrong way"), encoded
/// as UTF-16 for `CreateProcessW`.
fn encode_command_line(argv: &[String]) -> Vec<u16> {
    let mut line = String::new();
    for (i, arg) in argv.iter().enumerate() {
        if i != 0 {
            line.push(' ');
        }
        append_quoted(arg, &mut line);
    }
    line.encode_utf16().collect()
}

/// Appends `arg` to `line`, quoting and escaping only when necessary.
fn append_quoted(arg: &str, line: &mut String) {
    let needs_quotes = arg.is_empty() || arg.contains([' ', '\t', '\n', '\u{b}', '"']);
    if !needs_quotes {
        line.push_str(arg);
        return;
    }

    line.push('"');
    let mut backslashes = 0_usize;
    for ch in arg.chars() {
        match ch {
            '\\' => backslashes += 1,
            '"' => {
                // Double every preceding backslash, then add one to escape the
                // quote itself.
                line.push_str(&"\\".repeat(backslashes * 2 + 1));
                line.push('"');
                backslashes = 0;
            }
            other => {
                // Preceding backslashes are literal here, not escapes.
                line.push_str(&"\\".repeat(backslashes));
                backslashes = 0;
                line.push(other);
            }
        }
    }
    // Double any trailing backslashes so they don't escape the closing quote.
    line.push_str(&"\\".repeat(backslashes * 2));
    line.push('"');
}
