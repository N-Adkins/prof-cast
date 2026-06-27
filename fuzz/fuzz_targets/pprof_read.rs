#![no_main]

//! Fuzzes parsing of the pprof format. Raw bytes (gzip-framed or not) go
//! straight into `read`, which must never panic on arbitrary input.

use libfuzzer_sys::fuzz_target;
use profcast_formats::pprof::PprofFormat;

fuzz_target!(|data: &[u8]| {
    profcast_fuzz::check_read(&PprofFormat, data);
});
