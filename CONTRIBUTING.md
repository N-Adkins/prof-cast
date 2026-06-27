# Contributing

Contributions are welcome. This document describes the checks a change must
pass before it is merged.

## Toolchain

A stable Rust toolchain is required for everyday work. Miri and the fuzz targets
additionally require a nightly toolchain.

The minimum supported Rust version (MSRV) is 1.85, which CI verifies on every
pull request. Avoid language or dependency features that would raise it.

## Checks

CI runs the following on every pull request. Run them locally before pushing:

```sh
cargo fmt --all
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo build --workspace --all-features --locked
cargo test --workspace --all-features --locked
cargo check --workspace --no-default-features --locked
```

If you change `profcast-ffi`, regenerate the C header and confirm it is in sync:

```sh
make check-header
```

The committed header at `include/profcast.h` must match the generated output.

Optionally, run the test suite under Miri to catch undefined behaviour, and
exercise the fuzz targets:

```sh
make miri-setup    # one-time: install the nightly miri component
make miri
make fuzz target=<fuzz-target>
```

See `fuzz/README.md` for the available fuzz targets.

## Pull requests

Keep changes focused and explain the motivation in the description. New input or
output formats should include tests and, where applicable, a fuzz target.

## License

By contributing, you agree that your contributions are licensed under the same
terms as the project: the Apache License, Version 2.0 or the MIT license, at the
user's option.
