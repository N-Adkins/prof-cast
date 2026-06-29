//! A thin, self-contained wrapper over `perf_event_open(2)` for sampling.
//!
//! Safety here rests entirely on the kernel ABI: the `perf_event_attr` layout,
//! the `perf_event_mmap_page` control header at the start of the mmap, and the
//! `PERF_RECORD_SAMPLE` record format. Those are stable, documented contracts,
//! so the granular unsafe-hygiene lints are relaxed for this one module - the
//! interesting invariants are the ABI ones described inline, not per-deref
//! bookkeeping. The data area is read-only to us and only after an `Acquire`
//! fence against the kernel's published `data_head`.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::cast_ptr_alignment,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::multiple_unsafe_ops_per_block,
    clippy::undocumented_unsafe_blocks
)]

use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::sync::atomic::{Ordering, fence};
use std::{io, mem, ptr};

use profcast_core::{ProfcastError, Result};

// `perf_event_attr.type` / `.config` selecting the software cpu-clock event,
// which samples wall-clock time the task spends on a CPU.
const PERF_TYPE_SOFTWARE: u32 = 1;
const PERF_COUNT_SW_CPU_CLOCK: u64 = 0;

/// `sample_type` bit requesting a call-chain (instruction pointers) per sample.
const PERF_SAMPLE_CALLCHAIN: u64 = 1 << 5;

// Bits within the `perf_event_attr` flags bitfield word, in declaration order
// (least-significant bit first, as the kernel lays them out on little-endian).
// Note: `inherit` is deliberately not set - it is incompatible with giving each
// event its own mmap ring buffer, so threads are sampled individually instead
// (the caller enumerates them) and threads spawned mid-capture are not followed.
const ATTR_DISABLED: u64 = 1 << 0;
const ATTR_EXCLUDE_KERNEL: u64 = 1 << 5;
const ATTR_EXCLUDE_HV: u64 = 1 << 6;
const ATTR_FREQ: u64 = 1 << 10;
const ATTR_ENABLE_ON_EXEC: u64 = 1 << 12;

/// `ioctl` request to enable a (disabled) event. `_IO('$', 0)`.
const PERF_EVENT_IOC_ENABLE: libc::c_ulong = 0x2400;
/// `ioctl` request to disable an event. `_IO('$', 1)`.
const PERF_EVENT_IOC_DISABLE: libc::c_ulong = 0x2401;

/// Record type for a sample, carrying the requested call-chain.
const PERF_RECORD_SAMPLE: u32 = 9;

/// Instruction pointers at or above this value are `PERF_CONTEXT_*` markers
/// (e.g. user/kernel boundary), not real addresses, and are skipped.
const PERF_CONTEXT_MAX: u64 = u64::MAX - 4095;

/// Byte offsets of the fields we read in the `perf_event_mmap_page` control
/// header at the start of the mmap.
const OFF_DATA_HEAD: usize = 1024;
const OFF_DATA_TAIL: usize = 1032;
const OFF_DATA_OFFSET: usize = 1040;
const OFF_DATA_SIZE: usize = 1048;

/// Number of data pages in each ring buffer (must be a power of two). Larger
/// buffers tolerate longer gaps between drains before the kernel drops samples.
const DATA_PAGES: usize = 64;

/// Subset of `perf_event_attr` we populate; the rest stays zeroed. The flags
/// bitfield is modelled as a single `flags` word (see the `ATTR_*` bits).
#[repr(C)]
#[derive(Debug, Default)]
struct PerfEventAttr {
    type_: u32,
    size: u32,
    config: u64,
    sample_period_or_freq: u64,
    sample_type: u64,
    read_format: u64,
    flags: u64,
    wakeup_events: u32,
    bp_type: u32,
    bp_addr_or_config1: u64,
    bp_len_or_config2: u64,
    branch_sample_type: u64,
    sample_regs_user: u64,
    sample_stack_user: u32,
    clockid: i32,
    sample_regs_intr: u64,
    aux_watermark: u32,
    sample_max_stack: u16,
    reserved_2: u16,
    aux_sample_size: u32,
    reserved_3: u32,
}

/// A single open sampling event plus its mmap'd ring buffer.
///
/// On drop the buffer is unmapped and the file descriptor closed (the latter
/// via [`OwnedFd`]).
pub(super) struct Counter {
    fd: OwnedFd,
    base: *mut u8,
    mmap_len: usize,
    data_offset: u64,
    data_size: u64,
}

impl Counter {
    /// Opens a cpu-clock sampling event for thread `tid` at `frequency_hz` and
    /// maps its ring buffer. `tid == 0` means the calling thread.
    ///
    /// With `enable_on_exec` the event stays disabled until the target's next
    /// `execve`, so a freshly launched (and still pre-`exec`) child is sampled
    /// from its first instruction; the caller must then not [`enable`] it
    /// manually. Otherwise the event must be started with [`enable`].
    ///
    /// # Errors
    ///
    /// Returns [`ProfcastError::Capture`] if `perf_event_open` or the mmap
    /// fails (commonly `EACCES`/`EPERM` from `perf_event_paranoid`, or the
    /// target thread having exited).
    ///
    /// [`enable`]: Counter::enable
    pub(super) fn open(tid: u32, frequency_hz: u32, enable_on_exec: bool) -> Result<Self> {
        let mut flags = ATTR_DISABLED | ATTR_EXCLUDE_KERNEL | ATTR_EXCLUDE_HV | ATTR_FREQ;
        if enable_on_exec {
            flags |= ATTR_ENABLE_ON_EXEC;
        }
        let mut attr = PerfEventAttr {
            type_: PERF_TYPE_SOFTWARE,
            config: PERF_COUNT_SW_CPU_CLOCK,
            sample_period_or_freq: u64::from(frequency_hz),
            sample_type: PERF_SAMPLE_CALLCHAIN,
            flags,
            ..PerfEventAttr::default()
        };
        attr.size = mem::size_of::<PerfEventAttr>() as u32;

        // SAFETY: `attr` points to a valid, fully-initialized struct of
        // `attr.size` bytes; `tid` is a thread id (or 0 for self); cpu -1 means
        // "any CPU"; no group fd; no flags.
        let raw = unsafe {
            libc::syscall(
                libc::SYS_perf_event_open,
                ptr::from_ref(&attr),
                tid as libc::pid_t,
                -1_i32 as libc::c_int,
                -1_i32 as libc::c_int,
                0_u64 as libc::c_ulong,
            )
        };
        if raw < 0 {
            return Err(capture_errno("perf_event_open", tid));
        }
        // SAFETY: a non-negative syscall return is a fresh, owned fd.
        let fd = unsafe { OwnedFd::from_raw_fd(raw as RawFd) };

        let page_size = page_size();
        let mmap_len = (1 + DATA_PAGES) * page_size;
        // SAFETY: a null hint lets the kernel choose the address; the length is
        // a multiple of the page size; the fd is the event we just opened.
        let base = unsafe {
            libc::mmap(
                ptr::null_mut(),
                mmap_len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                fd.as_raw_fd(),
                0,
            )
        };
        if base == libc::MAP_FAILED {
            return Err(capture_errno("mmap perf ring", tid));
        }
        let base = base.cast::<u8>();

        // The kernel publishes where the data area starts and how big it is.
        let data_offset = unsafe { read_meta(base, OFF_DATA_OFFSET) };
        let data_size = unsafe { read_meta(base, OFF_DATA_SIZE) };

        Ok(Self {
            fd,
            base,
            mmap_len,
            data_offset,
            data_size,
        })
    }

    /// Enables sampling on this event.
    ///
    /// # Errors
    ///
    /// Returns [`ProfcastError::Capture`] if the `ioctl` fails.
    pub(super) fn enable(&self) -> Result<()> {
        self.ioctl(PERF_EVENT_IOC_ENABLE, "enable")
    }

    /// Disables sampling on this event.
    ///
    /// # Errors
    ///
    /// Returns [`ProfcastError::Capture`] if the `ioctl` fails.
    pub(super) fn disable(&self) -> Result<()> {
        self.ioctl(PERF_EVENT_IOC_DISABLE, "disable")
    }

    fn ioctl(&self, request: libc::c_ulong, what: &str) -> Result<()> {
        // SAFETY: a valid event fd and a no-argument perf ioctl request.
        let rc = unsafe { libc::ioctl(self.fd.as_raw_fd(), request as _, 0) };
        if rc < 0 {
            return Err(ProfcastError::Capture(format!(
                "perf ioctl {what} failed: {}",
                io::Error::last_os_error()
            )));
        }
        Ok(())
    }

    /// Drains every complete sample currently in the ring buffer, invoking
    /// `on_sample` with each call-chain. Instruction pointers are passed
    /// leaf-first (as perf reports them), with `PERF_CONTEXT_*` markers removed.
    pub(super) fn drain(&self, mut on_sample: impl FnMut(&[u64])) {
        let data = unsafe { self.base.add(self.data_offset as usize) };
        let size = self.data_size;
        if size == 0 {
            return;
        }

        let head = unsafe { read_meta(self.base, OFF_DATA_HEAD) };
        // Order the reads of record bytes after the load of `data_head`.
        fence(Ordering::Acquire);
        let mut tail = unsafe { read_meta(self.base, OFF_DATA_TAIL) };

        let mut ips: Vec<u64> = Vec::new();
        while tail < head {
            let mut header = [0_u8; 8];
            copy_from_ring(data, size, tail, &mut header);
            let record_type = u32::from_ne_bytes([header[0], header[1], header[2], header[3]]);
            let record_size = u64::from(u16::from_ne_bytes([header[6], header[7]]));
            if record_size == 0 {
                break; // Defensive: a zero-length record would loop forever.
            }

            if record_type == PERF_RECORD_SAMPLE {
                // Body layout for sample_type == CALLCHAIN: u64 nr, then nr u64s.
                let mut count = [0_u8; 8];
                copy_from_ring(data, size, tail + 8, &mut count);
                let mut nr = u64::from_ne_bytes(count);
                // Clamp against the record's own size in case of corruption
                // (each ip is 8 bytes, hence the shift instead of a divide).
                nr = nr.min(record_size.saturating_sub(16) >> 3);

                ips.clear();
                for i in 0..nr {
                    let mut ip = [0_u8; 8];
                    copy_from_ring(data, size, tail + 16 + i * 8, &mut ip);
                    let ip = u64::from_ne_bytes(ip);
                    if ip < PERF_CONTEXT_MAX {
                        ips.push(ip);
                    }
                }
                if !ips.is_empty() {
                    on_sample(&ips);
                }
            }

            tail += record_size;
        }

        // Publish how far we consumed, after the reads above.
        fence(Ordering::Release);
        unsafe { write_meta(self.base, OFF_DATA_TAIL, tail) };
    }
}

impl Drop for Counter {
    fn drop(&mut self) {
        // SAFETY: `base`/`mmap_len` are the mapping returned by `open`; the fd
        // is closed separately by `OwnedFd`'s own drop after this.
        unsafe {
            libc::munmap(self.base.cast::<libc::c_void>(), self.mmap_len);
        }
    }
}

/// Reads a `u64` control field from the metadata page at `offset`.
unsafe fn read_meta(base: *const u8, offset: usize) -> u64 {
    unsafe { base.add(offset).cast::<u64>().read_volatile() }
}

/// Writes a `u64` control field (the consumer tail) at `offset`.
unsafe fn write_meta(base: *mut u8, offset: usize, value: u64) {
    unsafe { base.add(offset).cast::<u64>().write_volatile(value) }
}

/// Copies `out.len()` bytes out of the ring starting at logical position `pos`,
/// wrapping around the power-of-two `size` boundary as needed.
fn copy_from_ring(data: *const u8, size: u64, pos: u64, out: &mut [u8]) {
    let mut p = pos % size;
    for slot in out.iter_mut() {
        *slot = unsafe { data.add(p as usize).read_volatile() };
        p += 1;
        if p == size {
            p = 0;
        }
    }
}

fn page_size() -> usize {
    // SAFETY: `sysconf` with a valid name has no preconditions.
    let raw = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    if raw > 0 { raw as usize } else { 4096 }
}

/// Builds a [`ProfcastError::Capture`] from the current `errno`, with a hint for
/// the common permission case.
fn capture_errno(op: &str, tid: u32) -> ProfcastError {
    let err = io::Error::last_os_error();
    let hint = if matches!(err.raw_os_error(), Some(libc::EACCES | libc::EPERM)) {
        " (try lowering /proc/sys/kernel/perf_event_paranoid, or run with more privileges)"
    } else {
        ""
    };
    ProfcastError::Capture(format!("{op} failed for thread {tid}: {err}{hint}"))
}

// SAFETY: `Counter` owns a private mmap and fd; the raw pointer is only ever
// dereferenced behind `&self` on the owning thread. Sending one between threads
// (e.g. moving it into a collection) is sound.
unsafe impl Send for Counter {}
