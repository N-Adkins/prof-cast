#![no_main]

//! Fuzzes detection of the folded format. The structured input lets the fuzzer
//! choose an optional filename (driving the extension heuristic) alongside the
//! probed bytes.

use libfuzzer_sys::fuzz_target;
use profcast_formats::folded::FoldedFormat;

fuzz_target!(|input: (Option<String>, Vec<u8>)| {
    let (filename, buf) = input;
    profcast_fuzz::check_probe(&FoldedFormat, filename.as_deref(), &buf);
});
