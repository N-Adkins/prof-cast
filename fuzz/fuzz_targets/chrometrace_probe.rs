#![no_main]

//! Fuzzes detection of the chrome trace format. The structured input lets the
//! fuzzer choose an optional filename (driving the extension heuristic)
//! alongside the probed bytes.

use libfuzzer_sys::fuzz_target;
use profcast_formats::chrometrace::ChromeTraceFormat;

fuzz_target!(|input: (Option<String>, Vec<u8>)| {
    let (filename, buf) = input;
    profcast_fuzz::check_probe(&ChromeTraceFormat, filename.as_deref(), &buf);
});
