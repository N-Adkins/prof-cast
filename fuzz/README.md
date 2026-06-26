# Fuzzing

Coverage-guided fuzz targets for profcast's input formats, built on
[`cargo-fuzz`](https://github.com/rust-fuzz/cargo-fuzz) and libFuzzer. This crate
is excluded from the main workspace, as it requires a nightly toolchain and
sanitizer flags.

## Layout

- `src/lib.rs` holds the shared harness. `check_probe`, `check_read`, and
  `check_all` drive any `InputFormat` and assert the cross-format invariants.
- `fuzz_targets/` holds one thin target per entry point:
  - `folded_read` feeds raw bytes to `FoldedFormat::read`.
  - `folded_probe` feeds a `(filename, bytes)` pair to `FoldedFormat::probe`.
  - `c_api` drives the exported C ABI, including null handles and ownership.

## Running

```sh
make fuzz-install          # one-time: cargo install cargo-fuzz
cargo fuzz list            # list targets
make fuzz t=folded_read    # run a target
```

## Adding a format

1. Implement `InputFormat` and register it in `Registry::with_builtins`.
2. Add `fuzz_targets/<format>_read.rs` and `<format>_probe.rs` calling
   `profcast_fuzz::check_read` and `check_probe`.
3. Declare the new `[[bin]]` entries in `Cargo.toml`.

No harness changes are needed; the invariant checks apply to every format.
