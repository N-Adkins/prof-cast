#![no_main]

//! Fuzzes parsing of the folded format. Raw bytes go straight into `read`.

use libfuzzer_sys::fuzz_target;
use profcast_formats::folded::FoldedFormat;

fuzz_target!(|data: &[u8]| {
    profcast_fuzz::check_read(&FoldedFormat, data);
});
