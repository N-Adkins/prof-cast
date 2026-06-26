//! Reusable fuzzing harness for profcast input formats.
//!
//! Every format's fuzz targets live in `fuzz_targets/*.rs` and are intentionally
//! trivial: they hand a format and some bytes to the helpers here. The actual
//! invariant checks live in this crate so that each format - present and future
//! - is held to exactly the same contract. Adding a format means writing two
//! small targets that call [`check_probe`] and [`check_read`]; no new harness
//! logic required.

use std::{ffi::CStr, ptr};

use arbitrary::Arbitrary;
use profcast_core::{
    format::{InputFormat, ProbeData},
    model::Profile,
};
use profcast_ffi::{
    profcast_last_error, profcast_probe, profcast_profile_free, profcast_profile_to_json,
    profcast_read, profcast_string_free, profcast_version,
};

#[derive(Debug, Arbitrary)]
pub enum FuzzBuffer {
    Bytes(Vec<u8>),
    NullEmpty,
}

impl FuzzBuffer {
    fn parts(&self) -> (*const u8, usize) {
        match self {
            Self::Bytes(bytes) => (bytes.as_ptr(), bytes.len()),
            Self::NullEmpty => (ptr::null(), 0),
        }
    }
}

#[derive(Debug)]
struct FuzzCString {
    bytes: Vec<u8>,
}

impl FuzzCString {
    fn new(mut bytes: Vec<u8>) -> Self {
        bytes.push(0);
        Self { bytes }
    }

    fn as_ptr(&self) -> *const std::ffi::c_char {
        self.bytes.as_ptr().cast()
    }
}

fn make_c_string(bytes: Option<Vec<u8>>) -> Option<FuzzCString> {
    bytes.map(FuzzCString::new)
}

fn optional_c_string_ptr(value: &Option<FuzzCString>) -> *const std::ffi::c_char {
    value.as_ref().map_or_else(ptr::null, FuzzCString::as_ptr)
}

fn check_last_error_is_c_string() {
    let error = profcast_last_error();
    if !error.is_null() {
        // SAFETY: profcast_last_error returns either null or a library-owned
        // NUL-terminated string that stays valid until the next FFI call.
        let _ = unsafe { CStr::from_ptr(error) };
    }
}

#[derive(Debug, Arbitrary)]
pub struct CApiInput {
    pub input: FuzzBuffer,
    pub format: Option<Vec<u8>>,
    pub filename: Option<Vec<u8>>,
}

/// Fuzzes a format's [`InputFormat::probe`].
///
/// Probing is a best-effort inspection of untrusted bytes, so the only contract
/// we can assert is that it never panics, hangs, or reads out of bounds (the
/// sanitizer catches the latter two). `filename` lets the fuzzer also drive the
/// extension-sniffing path.
pub fn check_probe(format: &dyn InputFormat, filename: Option<&str>, buf: &[u8]) {
    let data = ProbeData { filename, buf };
    let _ = format.probe(&data);
}

/// Fuzzes a format's [`InputFormat::read`].
///
/// `read` must never panic on arbitrary input - it either rejects the bytes
/// with an error or returns a profile that passes
/// [`Profile::validate`](profcast_core::model::Profile::validate). Reading is
/// also required to be deterministic.
pub fn check_read(format: &dyn InputFormat, buf: &[u8]) {
    let Ok(profile) = format.read(buf) else {
        return;
    };

    if let Err(error) = profile.validate() {
        panic!(
            "format '{}' produced an invalid profile: {error}",
            format.name()
        );
    }

    // Parsing identical bytes twice must yield an identical profile.
    match format.read(buf) {
        Ok(second) => assert!(
            profile == second,
            "format '{}' parsed identical input into different profiles",
            format.name(),
        ),
        Err(error) => panic!(
            "format '{}' parsed input once but failed on the identical retry: {error}",
            format.name(),
        ),
    }
}

/// Runs both [`check_probe`] and [`check_read`] for a format.
pub fn check_all(format: &dyn InputFormat, filename: Option<&str>, buf: &[u8]) {
    check_probe(format, filename, buf);
    check_read(format, buf);
}

/// Fuzzes the exported C API using contract-valid pointer arguments.
///
/// This intentionally does not invent arbitrary invalid addresses: passing
/// invalid pointers to an `unsafe extern "C"` function is caller-side UB, not a
/// recoverable profcast error. It does cover valid byte buffers, null/empty
/// buffers, optional C strings with arbitrary bytes, null handles, ownership,
/// JSON output, and profile invariants.
pub fn check_c_api(input: CApiInput) {
    let format = make_c_string(input.format);
    let filename = make_c_string(input.filename);
    let format = optional_c_string_ptr(&format);
    let filename = optional_c_string_ptr(&filename);
    let (buf, len) = input.input.parts();

    let version = profcast_version();
    assert!(!version.is_null(), "profcast_version returned null");
    // SAFETY: profcast_version returns a static NUL-terminated string.
    let _ = unsafe { CStr::from_ptr(version) };

    // These null cases are explicitly supported no-ops/errors.
    // SAFETY: null is documented as a no-op for free functions.
    unsafe {
        profcast_string_free(ptr::null_mut());
        profcast_profile_free(ptr::null_mut());
    }
    // SAFETY: null profile pointers are documented as rejected with null.
    let null_json = unsafe { profcast_profile_to_json(ptr::null()) };
    assert!(
        null_json.is_null(),
        "serializing a null profile unexpectedly succeeded",
    );
    check_last_error_is_c_string();

    // SAFETY: buf/len are either a valid slice pair or the documented
    // null-empty pair. filename is either null or a live NUL-terminated string.
    let detected = unsafe { profcast_probe(buf, len, filename) };
    if detected.is_null() {
        check_last_error_is_c_string();
    } else {
        // SAFETY: non-null probe results are owned NUL-terminated strings.
        let detected_name = unsafe { CStr::from_ptr(detected) };
        assert!(
            detected_name.to_str().is_ok(),
            "probe returned a non-UTF-8 format name",
        );
        // SAFETY: detected was returned by profcast_probe and has not been freed.
        unsafe { profcast_string_free(detected) };
    }

    // SAFETY: buf/len are either a valid slice pair or the documented
    // null-empty pair. format/filename are either null or live C strings.
    let profile = unsafe { profcast_read(buf, len, format, filename) };
    if profile.is_null() {
        check_last_error_is_c_string();
        return;
    }

    // SAFETY: profile is a live handle returned by profcast_read.
    let json = unsafe { profcast_profile_to_json(profile) };
    assert!(!json.is_null(), "serializing a live profile failed");

    // SAFETY: json is an owned NUL-terminated string returned by profcast.
    let json_text = unsafe { CStr::from_ptr(json) }
        .to_str()
        .expect("profile JSON was not UTF-8");
    let parsed: Profile = serde_json::from_str(json_text).expect("profile JSON was invalid");
    parsed
        .validate()
        .expect("C API returned an invalid profile");

    // SAFETY: both pointers were returned by profcast and have not been freed.
    unsafe {
        profcast_string_free(json);
        profcast_profile_free(profile);
    }
}
