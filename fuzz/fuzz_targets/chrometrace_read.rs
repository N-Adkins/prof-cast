#![no_main]

//! Fuzzes parsing of the chrome trace format. Raw bytes go straight into `read`.

use libfuzzer_sys::fuzz_target;
use profcast_formats::chrometrace::ChromeTraceFormat;

fuzz_target!(|data: &[u8]| {
    profcast_fuzz::check_read(&ChromeTraceFormat, data);
});
