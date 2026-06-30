//! Best-effort symbolization of instruction pointers via `DbgHelp`.
//!
//! A single `DbgHelp` session is initialized against the target process; it both
//! backs the `StackWalk64` function-table lookups during sampling and resolves
//! addresses to `function`/`file`/`line`/`module` afterwards. `DbgHelp` is not
//! thread-safe, but every call here happens on the single capture thread, so no
//! external locking is needed. The granular unsafe-hygiene lints are relaxed for
//! the module as in the sibling FFI modules.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_ptr_alignment,
    clippy::multiple_unsafe_ops_per_block,
    clippy::undocumented_unsafe_blocks
)]

use std::mem;
use std::ptr;
use std::time::{Duration, Instant};

use profcast_core::{ProfcastError, Result};
use profcast_core::model::Frame;

use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::System::Diagnostics::Debug::{
    IMAGEHLP_LINE64, IMAGEHLP_MODULE64, SYMBOL_INFO, SymCleanup, SymFromAddr,
    SymGetLineFromAddr64, SymGetModuleInfo64, SymInitialize, SymRefreshModuleList, SymSetOptions,
};

// `SymSetOptions` flags, declared locally to stay independent of which
// `windows-sys` feature exports each constant.
const SYMOPT_UNDNAME: u32 = 0x0000_0002;
const SYMOPT_DEFERRED_LOADS: u32 = 0x0000_0004;
const SYMOPT_LOAD_LINES: u32 = 0x0000_0010;
const SYMOPT_FAIL_CRITICAL_ERRORS: u32 = 0x0000_0200;

/// `SYMBOL_INFO.Flags` bit marking a symbol synthesized from the export table.
const SYMFLAG_EXPORT: u32 = 0x0000_0200;

/// Largest displacement past an export (one with no known size) we still trust
/// as that function, rather than an unexported neighbor.
const EXPORT_MAX_DISPLACEMENT: u64 = 0x2000;

/// Maximum symbol-name length (in bytes) we make room for, per `DbgHelp`'s
/// `MAX_SYM_NAME`.
const MAX_SYM_NAME: usize = 2000;

/// How long to coast between `SymRefreshModuleList` calls; refreshing every tick
/// would be needlessly heavy, but a launched target loads DLLs as it runs.
const REFRESH_INTERVAL: Duration = Duration::from_millis(150);

/// `SYMBOL_INFO` immediately followed by its variable-length name buffer, so the
/// name characters `DbgHelp` writes past the struct's `Name[1]` stay in bounds.
#[repr(C)]
struct SymbolBuffer {
    info: SYMBOL_INFO,
    _name: [u8; MAX_SYM_NAME],
}

/// Owns a `DbgHelp` session for one profiled process.
pub(super) struct Symbolizer {
    process: HANDLE,
    last_refresh: Option<Instant>,
}

impl Symbolizer {
    /// Initializes `DbgHelp` for `process`.
    ///
    /// When `invade` is set, the process's currently-loaded modules are
    /// enumerated up front. That must be `false` for a process created
    /// suspended: its loader has not run, so there are no modules to walk yet
    /// and enumeration fails - [`refresh_modules`](Symbolizer::refresh_modules)
    /// picks them up once the target is resumed.
    ///
    /// # Errors
    ///
    /// Returns [`ProfcastError::Capture`] if `SymInitialize` fails.
    pub(super) fn new(process: HANDLE, invade: bool) -> Result<Self> {
        // SAFETY: setting global DbgHelp options has no preconditions.
        unsafe {
            SymSetOptions(
                SYMOPT_UNDNAME | SYMOPT_DEFERRED_LOADS | SYMOPT_LOAD_LINES
                    | SYMOPT_FAIL_CRITICAL_ERRORS,
            );
        }
        // SAFETY: `process` is a valid handle; a null search path uses the
        // default; the invade flag enumerates already-loaded modules.
        let ok = unsafe { SymInitialize(process, ptr::null(), i32::from(invade)) };
        if ok == 0 {
            return Err(ProfcastError::Capture(format!(
                "SymInitialize failed: {}",
                std::io::Error::last_os_error()
            )));
        }
        tracing::debug!("DbgHelp session initialized");
        Ok(Self {
            process,
            last_refresh: None,
        })
    }

    /// Refreshes `DbgHelp`'s module list if [`REFRESH_INTERVAL`] has elapsed since
    /// the last refresh, so DLLs loaded after `SymInitialize` become known.
    pub(super) fn refresh_modules(&mut self) {
        let now = Instant::now();
        if self
            .last_refresh
            .is_some_and(|last| now.duration_since(last) < REFRESH_INTERVAL)
        {
            return;
        }
        self.last_refresh = Some(now);
        // SAFETY: refreshing the module list of our own DbgHelp session.
        let ok = unsafe { SymRefreshModuleList(self.process) };
        if ok == 0 {
            tracing::trace!(
                error = %std::io::Error::last_os_error(),
                "SymRefreshModuleList failed; module set may be stale",
            );
        }
    }

    /// Turns a raw instruction pointer into a [`Frame`], filling in whatever
    /// `DbgHelp` can resolve and degrading to the bare address otherwise.
    pub(super) fn resolve(&self, ip: u64) -> Frame {
        let function = self.function_at(ip);
        let module = self.module_at(ip);
        let (file, line) = self.line_at(ip);
        let raw = function.clone().unwrap_or_else(|| format!("0x{ip:x}"));
        Frame {
            raw,
            function,
            file,
            line,
            module,
            address: Some(ip),
        }
    }

    /// Resolves the function name covering `ip`, if any.
    fn function_at(&self, ip: u64) -> Option<String> {
        let mut buffer: SymbolBuffer = unsafe { mem::zeroed() };
        buffer.info.SizeOfStruct = mem::size_of::<SYMBOL_INFO>() as u32;
        buffer.info.MaxNameLen = MAX_SYM_NAME as u32;
        let mut displacement: u64 = 0;

        // SAFETY: `buffer.info` is a correctly-sized SYMBOL_INFO with room for
        // `MaxNameLen` trailing name bytes in the same allocation.
        let ok = unsafe {
            SymFromAddr(
                self.process,
                ip,
                ptr::from_mut(&mut displacement),
                ptr::from_mut(&mut buffer.info),
            )
        };
        if ok == 0 {
            return None;
        }

        // Without PDBs, DbgHelp returns the nearest export at or below `ip`, so
        // require `ip` to land within the symbol's extent (mirroring the Linux
        // size bound). Export symbols carry no extent (`Size == 0`); past a
        // generous distance they are almost certainly a different function.
        let size = u64::from(buffer.info.Size);
        if size != 0 && displacement >= size {
            return None;
        }
        if size == 0
            && (buffer.info.Flags & SYMFLAG_EXPORT) != 0
            && displacement > EXPORT_MAX_DISPLACEMENT
        {
            return None;
        }

        let len = (buffer.info.NameLen as usize).min(MAX_SYM_NAME);
        if len == 0 {
            return None;
        }
        // SAFETY: `Name` begins the trailing name buffer, contiguous with
        // `_name`; `len` bytes were written there and stay within the struct.
        let bytes =
            unsafe { std::slice::from_raw_parts(ptr::addr_of!(buffer.info.Name).cast::<u8>(), len) };
        Some(String::from_utf8_lossy(bytes).into_owned())
    }

    /// Resolves the owning module's short name for `ip`, if known.
    fn module_at(&self, ip: u64) -> Option<String> {
        let mut info: IMAGEHLP_MODULE64 = unsafe { mem::zeroed() };
        info.SizeOfStruct = mem::size_of::<IMAGEHLP_MODULE64>() as u32;
        // SAFETY: `info` is a correctly-sized IMAGEHLP_MODULE64.
        let ok = unsafe { SymGetModuleInfo64(self.process, ip, ptr::from_mut(&mut info)) };
        if ok == 0 {
            return None;
        }
        let name = c_str_from_array(&info.ModuleName);
        (!name.is_empty()).then_some(name)
    }

    /// Resolves the source file and line for `ip`, if line info is available.
    fn line_at(&self, ip: u64) -> (Option<String>, Option<u32>) {
        let mut line: IMAGEHLP_LINE64 = unsafe { mem::zeroed() };
        line.SizeOfStruct = mem::size_of::<IMAGEHLP_LINE64>() as u32;
        let mut displacement: u32 = 0;
        // SAFETY: `line` is a correctly-sized IMAGEHLP_LINE64.
        let ok = unsafe {
            SymGetLineFromAddr64(
                self.process,
                ip,
                ptr::from_mut(&mut displacement),
                ptr::from_mut(&mut line),
            )
        };
        if ok == 0 {
            return (None, None);
        }
        let file = if line.FileName.is_null() {
            None
        } else {
            // SAFETY: DbgHelp hands back a NUL-terminated ANSI string here.
            let s = unsafe { c_str_from_ptr(line.FileName) };
            (!s.is_empty()).then_some(s)
        };
        (file, Some(line.LineNumber))
    }
}

impl Drop for Symbolizer {
    fn drop(&mut self) {
        // SAFETY: tearing down the session we created in `new`.
        unsafe {
            SymCleanup(self.process);
        }
    }
}

/// Reads a NUL-terminated string from a fixed-size `CHAR` array (lossy UTF-8).
/// `CHAR` is signed in the bindings, so the bytes are reinterpreted as `u8`.
fn c_str_from_array(buf: &[i8]) -> String {
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    let bytes: Vec<u8> = buf.get(..end).unwrap_or(buf).iter().map(|&b| b as u8).collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

/// Reads a NUL-terminated ANSI string from a raw pointer (lossy UTF-8), bounding
/// the scan so a missing terminator cannot run away.
unsafe fn c_str_from_ptr(ptr: *const u8) -> String {
    const MAX: usize = 4096;
    let mut bytes = Vec::new();
    for i in 0..MAX {
        // SAFETY: the caller guarantees a valid NUL-terminated string; the bound
        // also stops us before `MAX` regardless.
        let b = unsafe { *ptr.add(i) };
        if b == 0 {
            break;
        }
        bytes.push(b);
    }
    String::from_utf8_lossy(&bytes).into_owned()
}
