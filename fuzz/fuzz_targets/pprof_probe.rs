#![no_main]

//! Fuzzes detection of the pprof format. The structured input lets the fuzzer
//! choose an optional filename (driving the extension heuristic) alongside the
//! probed bytes.

use libfuzzer_sys::fuzz_target;
use profcast_formats::pprof::PprofFormat;

fuzz_target!(|input: (Option<String>, Vec<u8>)| {
    let (filename, buf) = input;
    profcast_fuzz::check_probe(&PprofFormat, filename.as_deref(), &buf);
});
