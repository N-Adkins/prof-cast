# profcast

Profcast is a transcoder for profiling data, inspired by FFmpeg. It reads a
profile in one format, parses it into a common internal model, and writes it
back out in another representation.

## Status

Early development. The folded stack and pprof formats can be read and written,
and profiles can be emitted as JSON or speedscope JSON. Note that folded output
is lossy: it keeps only the first value series, since the format carries a single
weight per stack. Speedscope output instead emits one profile per value series,
sharing a single frame table, so every series is preserved. Further input and
output formats are planned.

In addition to transcoding existing files, profcast can capture profiles
directly. On Linux a `perf_event_open`-based sampling backend profiles a running
process and feeds the same model, so a capture can be written in any supported
output format. Capture is platform-gated and selected at runtime; other
platforms are planned.

## Layout

The project is a Cargo workspace:

- `profcast-cli` builds the `profcast` binary.
- `profcast-core` defines the internal profile model and shared APIs.
- `profcast-formats` implements format detection and parsing.
- `profcast-capture` implements live capture backends (Linux `perf`).
- `profcast-ffi` exposes a C ABI over the core library.

## Building

Requires a stable Rust toolchain. The minimum supported Rust version (MSRV) is
1.85.

```sh
cargo build --release
```

The CLI is written to `target/release/profcast`.

To install the `profcast` binary into Cargo's bin directory (`~/.cargo/bin`):

```sh
just install
```

This wraps `cargo install --path crates/profcast-cli`. Remove it again with
`just uninstall`. (Recipes use [`just`](https://github.com/casey/just); install
it with `cargo install just`.)

## Usage

Convert a profile to JSON:

```sh
profcast convert input.folded output.json
```

Print the parsed model to stdout:

```sh
profcast dump input.folded
```

The input format is auto-detected, or set explicitly with `--from`. Use `-` as
the input or output path to read from stdin or write to stdout. Pass `-v` for
more verbose logging.

List the formats profcast can read (`R`) and write (`W`):

```sh
profcast formats
```

Capture a running process and write the result in any output format (Linux):

```sh
profcast record --pid 1234 --duration 5 profile.folded
```

Sampling runs until the target exits, or for `--duration` seconds if given;
`--freq` sets the sampling rate (default 99 Hz). The output format is inferred
from the extension or set with `--to`, exactly as for `convert`. Sampling its
own process via `--self` is supported mainly as a smoke test. The `perf` backend
needs `perf_event_open` access (see `/proc/sys/kernel/perf_event_paranoid`) and
resolves the deepest stacks when the target is built with frame pointers.

## C library

`profcast-ffi` builds as a static or shared library. The header is generated
with cbindgen and committed at `include/profcast.h`:

```sh
just header
```

## License

Licensed under either of the Apache License, Version 2.0 or the MIT license at
your option.
