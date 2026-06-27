//! End-to-end tests for the `profcast` binary.
//!
//! These drive the compiled CLI as a subprocess (via `CARGO_BIN_EXE_profcast`)
//! so that argument parsing, stdin/stdout handling, format inference, and exit
//! codes are exercised the way a user hits them - none of which the
//! library-level unit tests cover.
//!
//! These spawn the compiled binary as a subprocess and touch the filesystem,
//! neither of which Miri supports, so the whole suite is compiled out under
//! Miri rather than run there.
#![cfg(not(miri))]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

/// Path to the binary under test, provided by Cargo for integration tests.
const BIN: &str = env!("CARGO_BIN_EXE_profcast");

/// Returns a unique temp path scoped to this test process, for tests that need
/// a real file on disk (e.g. extension-based format inference).
fn temp_path(suffix: &str) -> PathBuf {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut path = std::env::temp_dir();
    path.push(format!("profcast-cli-{}-{n}-{suffix}", std::process::id()));
    path
}

/// Runs the CLI with `args`, feeding `stdin` to its standard input.
fn run_with_stdin(args: &[&str], stdin: &[u8]) -> Output {
    let mut child = Command::new(BIN)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn profcast");
    child
        .stdin
        .take()
        .expect("child stdin was not captured")
        .write_all(stdin)
        .expect("failed to write to child stdin");
    child
        .wait_with_output()
        .expect("failed to wait on profcast")
}

#[test]
fn convert_file_to_stdout_emits_pretty_json() {
    let input = temp_path("in.folded");
    std::fs::write(&input, "main;work 5\na;b;c 10\n").unwrap();

    let output = Command::new(BIN)
        .args(["convert", input.to_str().unwrap(), "-"])
        .output()
        .expect("failed to run profcast");
    std::fs::remove_file(&input).ok();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr),
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("\"frame_intern\""));
    assert!(stdout.contains("\"raw\": \"main\""));
    // Pretty output is indented across many lines.
    assert!(stdout.matches('\n').count() > 1);
}

#[test]
fn convert_infers_output_format_from_extension() {
    let input = temp_path("in.folded");
    let out = temp_path("out.json");
    std::fs::write(&input, "a;b 3\n").unwrap();

    let output = Command::new(BIN)
        .args(["convert", input.to_str().unwrap(), out.to_str().unwrap()])
        .output()
        .expect("failed to run profcast");

    let written = std::fs::read_to_string(&out).unwrap_or_default();
    std::fs::remove_file(&input).ok();
    std::fs::remove_file(&out).ok();

    assert!(output.status.success());
    assert!(written.contains("\"raw\": \"a\""));
}

#[test]
fn convert_folded_to_folded_round_trips() {
    let input = "main;work 5\na;b;c 10\n";
    let output = run_with_stdin(
        &["convert", "-", "-", "--from", "folded", "--to", "folded"],
        input.as_bytes(),
    );
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr),
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), input);
}

#[test]
fn convert_infers_folded_output_from_extension() {
    let input = temp_path("in.folded");
    let out = temp_path("out.folded");
    std::fs::write(&input, "a;b 3\n").unwrap();

    let output = Command::new(BIN)
        .args(["convert", input.to_str().unwrap(), out.to_str().unwrap()])
        .output()
        .expect("failed to run profcast");

    let written = std::fs::read_to_string(&out).unwrap_or_default();
    std::fs::remove_file(&input).ok();
    std::fs::remove_file(&out).ok();

    assert!(output.status.success());
    assert_eq!(written, "a;b 3\n");
}

#[test]
fn dump_compact_is_single_line() {
    let output = run_with_stdin(&["dump", "-", "--from", "folded", "--compact"], b"a;b 1\n");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    // Compact output is one JSON line plus the trailing newline the CLI adds.
    assert_eq!(stdout.matches('\n').count(), 1);
    assert!(stdout.contains("\"samples\""));
}

#[test]
fn reads_from_stdin_with_explicit_format() {
    let output = run_with_stdin(&["convert", "-", "-", "--from", "folded"], b"x;y 2\n");
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("\"raw\": \"x\""));
}

#[test]
fn unknown_input_format_fails() {
    let output = run_with_stdin(&["dump", "-", "--from", "nope"], b"a;b 1\n");
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("unknown input format"));
}

#[test]
fn unknown_output_format_fails() {
    let output = run_with_stdin(
        &["convert", "-", "-", "--from", "folded", "--to", "nope"],
        b"a;b 1\n",
    );
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("unknown output format"));
}

#[test]
fn undetectable_input_fails_with_hint() {
    let output = run_with_stdin(&["dump", "-"], b"this is not a profile at all\n");
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("could not detect input format"));
}
