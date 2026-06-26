# profcast fuzzing

Coverage-guided fuzz targets for profcast's input formats, built on
[`cargo-fuzz`](https://github.com/rust-fuzz/cargo-fuzz) / libFuzzer.

This crate is intentionally excluded from the main workspace (it needs a nightly
toolchain and sanitizer flags), so it has its own `Cargo.toml` and lock file.

## Layout

- `src/lib.rs` — the reusable harness. `check_probe`, `check_read`, and
  `check_all` drive any `InputFormat` and assert the cross-format invariants
  (model consistency + deterministic reads). Format-specific targets stay tiny.
- `fuzz_targets/` — one or two thin targets per format:
  - `folded_read` — feeds raw bytes to `FoldedFormat::read`.
  - `folded_probe` — feeds an arbitrary `(filename, bytes)` pair to
    `FoldedFormat::probe`.
  - `c_api` — drives the exported C ABI with C-style buffers/strings,
    null handles, JSON serialization, and ownership cleanup.

## Running

```sh
make fuzz-install              # one-time: cargo install cargo-fuzz
cargo fuzz list                # show available targets
make fuzz t=folded_read        # or: cargo +nightly fuzz run folded_read
make fuzz t=folded_probe
make fuzz t=c_api
```

## Adding a format

1. Implement `InputFormat` and register it in `Registry::with_builtins`.
2. Add `fuzz_targets/<format>_read.rs` and `<format>_probe.rs` that call
   `profcast_fuzz::check_read` / `check_probe` with your format.
3. Declare the new `[[bin]]` entries in `Cargo.toml`.

No harness changes are needed — the invariant checks in `src/lib.rs` apply to
every format automatically.
