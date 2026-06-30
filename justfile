# profcast task runner — run `just` to list recipes.
# Install: cargo install just   (or: cargo binstall just)

set windows-shell := ["cmd.exe", "/c"]

PPROF_PROTO_DIR := "crates/profcast-formats/proto"
PPROF_GEN := "crates/profcast-formats/src/pprof/proto.gen.rs"

# Treat rustdoc warnings (broken links, bad HTML) as errors, matching CI's docs
# job. Only rustdoc and doctests read this, so exporting it globally is harmless.
export RUSTDOCFLAGS := "-D warnings"

# List available recipes
default:
    @just --list

# Pre-commit gate: everything CI checks except the heavier msrv/miri jobs.
# Mirrors the fmt, clippy, docs, no-default-features, test, and C-ABI jobs.
check: fmt-check clippy docs no-default-features test check-header

# The full CI surface: `check` plus the MSRV and Miri jobs.
ci: check msrv miri

# Format the whole workspace in place
fmt:
    cargo fmt --all

# Fail if anything is not formatted (CI's fmt job)
fmt-check:
    cargo fmt --all -- --check

# Deny-warnings Clippy across all targets and features (CI's clippy job)
clippy:
    cargo clippy --workspace --all-targets --all-features --locked -- -D warnings

# Build the API docs, failing on any rustdoc warning (CI's docs job)
docs:
    cargo doc --workspace --all-features --no-deps --locked

# Build and run the test suite (CI's test job)
test:
    cargo build --workspace --all-features --locked
    cargo test --workspace --all-features --locked

# Type-check with default features off (CI's no-default-features job)
no-default-features:
    cargo check --workspace --no-default-features --locked

# Build with the minimum supported Rust version (CI's msrv job)
msrv:
    cargo +1.85.0 build --workspace --all-features --locked

# Install cargo-semver-checks (also needs a nightly toolchain for rustdoc JSON)
semver-checks-install:
    cargo install cargo-semver-checks

# Check the public API for SemVer breakage vs a baseline git rev (default: main)
semver-checks rev="main":
    cargo semver-checks --workspace --baseline-rev {{ rev }}

# Regenerate the C header from the FFI crate
header:
    cbindgen crates/profcast-ffi --config cbindgen.toml --output include/profcast.h

# Fail if the committed header is stale (CI's C-ABI job)
check-header: header
    git diff --exit-code include/profcast.h

# Install the CLI from source
install:
    cargo install --path crates/profcast-cli --locked

# Uninstall the CLI
uninstall:
    cargo uninstall profcast-cli

# Build the native example workloads (see examples/justfile)
build-examples:
    cd examples && just build

# Install the nightly toolchain and Miri
miri-setup:
    rustup toolchain install nightly --profile minimal --component miri
    cargo +nightly miri setup

# Run the test suite under Miri
miri:
    cargo +nightly miri test --workspace --all-features --locked

# Install cargo-fuzz
fuzz-install:
    cargo install cargo-fuzz

# Build all fuzz targets
fuzz-build:
    cargo +nightly fuzz build

# List available fuzz targets
fuzz-list:
    cargo +nightly fuzz list

# Run a fuzz target: just fuzz <target> [secs]
fuzz target secs="":
    cargo +nightly fuzz run {{ target }} {{ if secs != "" { "-- -max_total_time=" + secs } else { "" } }}

# Regenerate the vendored pprof prost types.
#
# Needs protoc and protoc-gen-prost (cargo install protoc-gen-prost). The
# committed output is consumed verbatim by src/pprof/mod.rs, which supplies the
# module wrapper and lint allows.
[unix]
proto:
    #!/usr/bin/env bash
    set -euo pipefail
    tmp=$(mktemp -d)
    protoc --prost_out="$tmp" -I {{ PPROF_PROTO_DIR }} {{ PPROF_PROTO_DIR }}/profile.proto
    mv "$(find "$tmp" -name '*.rs')" {{ PPROF_GEN }}
    rm -rf "$tmp"
    echo "regenerated {{ PPROF_GEN }}"
