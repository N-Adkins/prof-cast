#![no_main]

//! Fuzzes the exported C ABI with valid C-style inputs and ownership paths.

use libfuzzer_sys::fuzz_target;

fuzz_target!(|input: profcast_fuzz::CApiInput| {
    profcast_fuzz::check_c_api(input);
});
