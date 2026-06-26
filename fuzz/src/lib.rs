//! Reusable fuzzing harness for profcast input formats.
//!
//! Every format's fuzz targets live in `fuzz_targets/*.rs` and are intentionally
//! trivial: they hand a format and some bytes to the helpers here. The actual
//! invariant checks live in this crate so that each format - present and future
//! - is held to exactly the same contract. Adding a format means writing two
//! small targets that call [`check_probe`] and [`check_read`]; no new harness
//! logic required.

use profcast_core::format::{InputFormat, ProbeData};

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
        panic!("format '{}' produced an invalid profile: {error}", format.name());
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
