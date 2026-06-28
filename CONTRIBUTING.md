# Contributing

Contributions are welcome. This document describes the checks a change must
pass before it is merged.

## Toolchain

A stable Rust toolchain is required for everyday work. Miri and the fuzz targets
additionally require a nightly toolchain.

Common tasks are wrapped in a [`just`](https://github.com/casey/just) recipe file
(`justfile`). Install it with `cargo install just` (or `cargo binstall just`) and
run `just` to list the available recipes.

The minimum supported Rust version (MSRV) is 1.85, which CI verifies on every
pull request. Avoid language or dependency features that would raise it.

## Checks

Before pushing, run the pre-commit gate, which mirrors every CI job except the
heavier MSRV and Miri runs (formatting, Clippy, docs, tests, the
no-default-features check, and the C-header sync check):

```sh
just check
```

Each underlying job is also available on its own — `just fmt-check`, `just
clippy`, `just docs`, `just test`, `just no-default-features`, and `just
check-header` — and `just fmt` formats the workspace in place. The committed C
header at `include/profcast.h` must match cbindgen's output; `just check-header`
regenerates it and fails if it has drifted.

To run the complete CI surface locally, including MSRV and Miri:

```sh
just ci
```

Optionally, run the test suite under Miri to catch undefined behaviour, and
exercise the fuzz targets:

```sh
just miri-setup    # one-time: install the nightly miri component
just miri
just fuzz <fuzz-target>
```

See `fuzz/README.md` for the available fuzz targets.

## SemVer

The public API is checked for accidental SemVer-breaking changes with
[`cargo-semver-checks`](https://github.com/obi1kenobi/cargo-semver-checks). On
pull requests an advisory CI job diffs the branch against the base commit; it
reports breakage but does not block merges, since the crate is pre-1.0 and
breaking changes are still expected. Run it locally against `main` with:

```sh
just semver-checks-install   # one-time: needs a nightly toolchain for rustdoc JSON
just semver-checks           # or: just semver-checks rev=<git-rev>
```

Once the API stabilizes for 1.0, promote the CI job to a required check.

## Pull requests

Keep changes focused and explain the motivation in the description. New input or
output formats should include tests and, where applicable, a fuzz target.

## License

By contributing, you agree that your contributions are licensed under the same
terms as the project: the Apache License, Version 2.0 or the MIT license, at the
user's option.
