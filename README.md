# profcast

Profcast is a transcoder for profiling data, inspired by FFmpeg. It reads a
profile in one format, parses it into a common internal model, and writes it
back out in another representation.

## Status

Early development. The folded stack format can be read, and profiles can be
emitted as JSON. Further input and output formats are planned.

## Layout

The project is a Cargo workspace:

- `profcast-cli` builds the `profcast` binary.
- `profcast-core` defines the internal profile model and shared APIs.
- `profcast-formats` implements format detection and parsing.
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
make install
```

This wraps `cargo install --path crates/profcast-cli`. Remove it again with
`make uninstall`.

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

## C library

`profcast-ffi` builds as a static or shared library. The header is generated
with cbindgen and committed at `include/profcast.h`:

```sh
make header
```

## License

Licensed under either of the Apache License, Version 2.0 or the MIT license at
your option.
