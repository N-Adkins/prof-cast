//! This crate defines the FFI ABI used for the libprofcast library.
//!
//! # Error handling
//!
//! Fallible functions return either `NULL` or a sentinel and record a
//! human-readable message retrievable via [`profcast_last_error`]. The error is
//! stored per-thread, so it must be read on the same thread that produced it,
//! before that thread makes another profcast call.
//!
//! # Ownership
//!
//! Any non-null `char *` returned by this library must be released with
//! [`profcast_string_free`], and any `profcast_Profile *` with
//! [`profcast_profile_free`]. Pointers passed in are borrowed, never freed, by
//! the callee.

use std::{
    cell::RefCell,
    ffi::{CStr, CString, c_char},
    ptr,
    sync::OnceLock,
};

use profcast_core::{format::ProbeData, model::Profile as CoreProfile};
use profcast_formats::Registry;

thread_local! {
    /// The most recent error message produced on the current thread.
    static LAST_ERROR: RefCell<Option<CString>> = const { RefCell::new(None) };
}

/// Stores `message` as the current thread's last error.
fn set_last_error(message: impl Into<Vec<u8>>) {
    // Replace any interior NUL bytes so the message always round-trips to C.
    let bytes: Vec<u8> = message
        .into()
        .into_iter()
        .map(|b| if b == 0 { b'?' } else { b })
        .collect();
    let cstring = CString::new(bytes).unwrap_or_default();
    LAST_ERROR.with(|slot| *slot.borrow_mut() = Some(cstring));
}

/// Allocates an owned C string the caller must release with
/// [`profcast_string_free`]. Returns `NULL` on allocation/encoding failure.
fn into_c_string(value: &str) -> *mut c_char {
    CString::new(value).map_or_else(
        |_| {
            set_last_error("value contained an interior NUL byte");
            ptr::null_mut()
        },
        CString::into_raw,
    )
}

/// Returns the process-wide registry of built-in formats.
fn registry() -> &'static Registry {
    static REGISTRY: OnceLock<Registry> = OnceLock::new();
    REGISTRY.get_or_init(Registry::with_builtins)
}

/// Borrows `len` bytes starting at `ptr` as a slice.
///
/// A null `ptr` or zero `len` yields an empty slice.
///
/// # Safety
///
/// When `ptr` is non-null and `len` is non-zero, `ptr` must point to at least
/// `len` initialized bytes that remain valid for the returned borrow.
unsafe fn slice_from_raw<'a>(ptr: *const u8, len: usize) -> &'a [u8] {
    if ptr.is_null() || len == 0 {
        return &[];
    }
    // SAFETY: guaranteed valid for `len` bytes by the caller's contract above.
    unsafe { std::slice::from_raw_parts(ptr, len) }
}

/// Borrows an optional, NUL-terminated C string as `&str`.
///
/// A null pointer maps to `None`; non-UTF-8 contents also map to `None`.
///
/// # Safety
///
/// When non-null, `ptr` must point to a valid NUL-terminated C string that
/// stays alive and unmodified for the returned borrow.
unsafe fn opt_str<'a>(ptr: *const c_char) -> Option<&'a str> {
    if ptr.is_null() {
        return None;
    }
    // SAFETY: the caller guarantees a valid NUL-terminated string.
    let cstr = unsafe { CStr::from_ptr(ptr) };
    cstr.to_str().ok()
}

/// Opaque handle to a parsed profile produced by [`profcast_read`].
pub struct Profile {
    inner: CoreProfile,
}

/// Returns a null-terminated string showing the program name and version,
/// eg. "profcast 0.1.0".
///
/// In the case of an internal library error for this, it will
/// return "profcast <unknown>".
#[unsafe(no_mangle)]
pub extern "C" fn profcast_version() -> *const c_char {
    static VERSION: OnceLock<Option<CString>> = OnceLock::new();
    VERSION
        .get_or_init(|| CString::new(profcast_core::VERSION_STRING).ok())
        .as_ref()
        .map_or_else(|| c"profcast <unknown>".as_ptr(), |cstr| cstr.as_ptr())
}

/// Returns the current thread's most recent error message, or `NULL` if none.
///
/// The returned pointer is owned by the library and remains valid until the
/// next profcast call on this thread; do not free it.
#[unsafe(no_mangle)]
pub extern "C" fn profcast_last_error() -> *const c_char {
    LAST_ERROR.with(|slot| {
        slot.borrow()
            .as_ref()
            .map_or(ptr::null(), |cstring| cstring.as_ptr())
    })
}

/// Detects the format of `len` bytes at `buf`, optionally hinted by `filename`.
///
/// Returns an owned format-name string (free with [`profcast_string_free`]), or
/// `NULL` if no format matched.
///
/// # Safety
///
/// `buf` must be valid for `len` bytes (or null with `len` 0). `filename`, when
/// non-null, must be a valid NUL-terminated C string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn profcast_probe(
    buf: *const u8,
    len: usize,
    filename: *const c_char,
) -> *mut c_char {
    // SAFETY: forwarding the caller's `buf`/`len` validity contract.
    let bytes = unsafe { slice_from_raw(buf, len) };
    // SAFETY: forwarding the caller's `filename` validity contract.
    let filename = unsafe { opt_str(filename) };

    let probe = ProbeData {
        filename,
        buf: bytes,
    };
    registry().probe(&probe).map_or_else(
        || {
            set_last_error("no known format matched the input");
            ptr::null_mut()
        },
        |matched| into_c_string(matched.format.name()),
    )
}

/// Parses `len` bytes at `buf` into a profile handle.
///
/// `format` selects an input format by name; when null the format is
/// auto-detected by probing, with `filename` used as an optional hint. Returns
/// a handle to free with [`profcast_profile_free`], or `NULL` on error (see
/// [`profcast_last_error`]).
///
/// # Safety
///
/// `buf` must be valid for `len` bytes (or null with `len` 0). `format` and
/// `filename`, when non-null, must be valid NUL-terminated C strings.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn profcast_read(
    buf: *const u8,
    len: usize,
    format: *const c_char,
    filename: *const c_char,
) -> *mut Profile {
    // SAFETY: forwarding the caller's `buf`/`len` validity contract.
    let bytes = unsafe { slice_from_raw(buf, len) };
    // SAFETY: forwarding the caller's `format` validity contract.
    let format_name = unsafe { opt_str(format) };
    // SAFETY: forwarding the caller's `filename` validity contract.
    let filename = unsafe { opt_str(filename) };

    let registry = registry();
    let format = if let Some(name) = format_name {
        let Some(format) = registry.by_name(name) else {
            set_last_error(format!("unknown input format '{name}'"));
            return ptr::null_mut();
        };
        format
    } else {
        let probe = ProbeData {
            filename,
            buf: bytes,
        };
        let Some(matched) = registry.probe(&probe) else {
            set_last_error("could not detect input format");
            return ptr::null_mut();
        };
        matched.format
    };

    match format.read(bytes) {
        Ok(inner) => Box::into_raw(Box::new(Profile { inner })),
        Err(error) => {
            set_last_error(error.to_string());
            ptr::null_mut()
        }
    }
}

/// Serializes a profile to a JSON string (free with [`profcast_string_free`]).
///
/// Returns `NULL` on error (see [`profcast_last_error`]).
///
/// # Safety
///
/// `profile` must be a non-null pointer returned by [`profcast_read`] that has
/// not yet been freed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn profcast_profile_to_json(profile: *const Profile) -> *mut c_char {
    if profile.is_null() {
        set_last_error("profile pointer was null");
        return ptr::null_mut();
    }
    // SAFETY: non-null and, per the contract, a live handle from profcast_read.
    let profile = unsafe { &*profile };

    match serde_json::to_string(&profile.inner) {
        Ok(json) => into_c_string(&json),
        Err(error) => {
            set_last_error(error.to_string());
            ptr::null_mut()
        }
    }
}

/// Frees a profile handle previously returned by [`profcast_read`].
///
/// Passing `NULL` is a no-op.
///
/// # Safety
///
/// `profile` must either be null or a pointer from [`profcast_read`] that has
/// not already been freed. It must not be used after this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn profcast_profile_free(profile: *mut Profile) {
    if profile.is_null() {
        return;
    }
    // SAFETY: the contract guarantees a unique, not-yet-freed handle from us.
    drop(unsafe { Box::from_raw(profile) });
}

/// Frees a string previously returned by this library.
///
/// Passing `NULL` is a no-op.
///
/// # Safety
///
/// `string` must either be null or a pointer returned by a profcast function
/// (e.g. [`profcast_probe`]) that has not already been freed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn profcast_string_free(string: *mut c_char) {
    if string.is_null() {
        return;
    }
    // SAFETY: the contract guarantees a string we handed out via into_raw.
    drop(unsafe { CString::from_raw(string) });
}

#[cfg(test)]
mod test {
    use std::ffi::CStr;

    use super::*;
    use anyhow::Result;

    #[test]
    fn version_is_nonnull() {
        assert_ne!(profcast_version(), std::ptr::null_mut());
    }

    #[test]
    fn version_is_correct() -> Result<()> {
        // SAFETY: needed to consume c pointer for test
        let ffi_cstr = unsafe { CStr::from_ptr(profcast_version()) }.to_str()?;
        assert_eq!(ffi_cstr, profcast_core::VERSION_STRING);
        Ok(())
    }

    #[test]
    fn probe_detects_folded() -> Result<()> {
        let input = b"a;b;c 10\n";
        // SAFETY: valid buffer, null (absent) filename.
        let name = unsafe { profcast_probe(input.as_ptr(), input.len(), ptr::null()) };
        assert!(!name.is_null());
        // SAFETY: just returned a valid owned string above.
        let detected = unsafe { CStr::from_ptr(name) }.to_str()?.to_owned();
        // SAFETY: freeing the string we own.
        unsafe { profcast_string_free(name) };
        assert_eq!(detected, "folded");
        Ok(())
    }

    #[test]
    fn read_and_serialize_roundtrip() -> Result<()> {
        let input = b"main;work 5\n";
        // SAFETY: valid buffer; null format triggers auto-detect; null filename.
        let profile =
            unsafe { profcast_read(input.as_ptr(), input.len(), ptr::null(), ptr::null()) };
        assert!(!profile.is_null());

        // SAFETY: profile is a live handle from profcast_read.
        let json = unsafe { profcast_profile_to_json(profile) };
        assert!(!json.is_null());
        // SAFETY: just produced a valid owned string.
        let text = unsafe { CStr::from_ptr(json) }.to_str()?.to_owned();
        assert!(text.contains("\"raw\":\"main\""));
        assert!(text.contains("\"raw\":\"work\""));

        // SAFETY: freeing resources we own, each exactly once.
        unsafe { profcast_string_free(json) };
        // SAFETY: freeing the profile handle exactly once.
        unsafe { profcast_profile_free(profile) };
        Ok(())
    }

    #[test]
    fn read_unknown_format_sets_error() {
        // SAFETY: empty buffer and a valid format name that no format matches.
        let profile = unsafe { profcast_read(ptr::null(), 0, c"nope".as_ptr(), ptr::null()) };
        assert!(profile.is_null());
        // SAFETY: reading the thread-local error pointer just set above.
        let err = unsafe { CStr::from_ptr(profcast_last_error()) };
        assert!(err.to_str().unwrap_or_default().contains("nope"));
    }
}
