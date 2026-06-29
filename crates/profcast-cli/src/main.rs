#![allow(missing_docs)]
// unimportant for cli structures
// The application boundary, not the library: failing fast is acceptable here,
// so the never-panic lints are relaxed for the two canonical "fine in a binary"
// cases. The library crates (core/formats/ffi) stay strict.
#![allow(clippy::unwrap_used, clippy::expect_used)]

//! CLI for profcast

use std::io::{Read, Write};
use std::time::Duration;

use anyhow::{Context, bail};
use clap::{Parser, Subcommand};
use profcast_capture::{CaptureSpec, Sources, Target};
use profcast_core::{
    format::{OutputFormat, ProbeData, WriteOptions},
    model::Profile,
};
use profcast_formats::Registry;
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(name = "profcast")]
#[command(version = profcast_core::VERSION)]
struct Cli {
    #[command(subcommand)]
    command: Command,
    #[arg(short, long, action = clap::ArgAction::Count)]
    verbose: u8,
}

#[derive(Subcommand)]
enum Command {
    /// Read a profile and transcode it to another representation.
    ///
    /// Use `-` for `input`/`output` to read from stdin / write to stdout.
    Convert {
        input: String,
        output: String,

        /// Input format name (e.g. `folded`). Auto-detected when omitted.
        #[arg(long)]
        from: Option<String>,

        /// Output format name (e.g. `json`). Inferred from the output file
        /// extension when omitted, falling back to `json`.
        #[arg(long)]
        to: Option<String>,
    },

    /// Sample a running process (or this one) into a profile and write it out.
    ///
    /// Use `-` for `output` to write to stdout.
    Record {
        output: String,

        /// PID of the process to profile.
        #[arg(long, conflicts_with_all = ["self_target", "program"])]
        pid: Option<u32>,

        /// Profile this `profcast` process itself (useful as a smoke test).
        #[arg(long = "self", conflicts_with = "program")]
        self_target: bool,

        /// Launch a program and profile it from start to exit, e.g.
        /// `--program "/usr/bin/grep -r foo ."`.
        #[arg(long)]
        program: Option<String>,

        /// Sampling frequency in hertz.
        #[arg(long, default_value_t = 99)]
        freq: u32,

        /// How long to sample, in seconds. Defaults to until the target exits
        /// (or a fixed window when profiling self).
        #[arg(long)]
        duration: Option<f64>,

        /// Capture backend name (e.g. `perf`). Defaults to the first available.
        #[arg(long)]
        source: Option<String>,

        /// Output format name (e.g. `folded`). Inferred from the output file
        /// extension when omitted, falling back to `json`.
        #[arg(long)]
        to: Option<String>,
    },

    /// Read a profile and print its parsed internal model as JSON to stdout.
    ///
    /// Use `-` for `input` to read from stdin.
    Dump {
        input: String,

        /// Input format name (e.g. `folded`). Auto-detected when omitted.
        #[arg(long)]
        from: Option<String>,

        /// Emit single-line JSON instead of indented output.
        #[arg(long)]
        compact: bool,
    },
}

fn init_logging(verbose: u8) {
    let default_level = match verbose {
        0 => "warn",
        1 => "info",
        2 => "debug",
        _ => "trace",
    };

    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default_level));

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();
}

/// Reads all bytes from a path, or from stdin when `path` is `-`.
fn read_input(path: &str) -> anyhow::Result<Vec<u8>> {
    if path == "-" {
        let mut buf = Vec::new();
        std::io::stdin()
            .read_to_end(&mut buf)
            .context("failed to read from stdin")?;
        Ok(buf)
    } else {
        std::fs::read(path).with_context(|| format!("failed to read input file '{path}'"))
    }
}

/// Writes bytes to a path, or to stdout when `path` is `-`.
fn write_output(path: &str, bytes: &[u8]) -> anyhow::Result<()> {
    if path == "-" {
        std::io::stdout()
            .write_all(bytes)
            .context("failed to write to stdout")
    } else {
        std::fs::write(path, bytes).with_context(|| format!("failed to write output file '{path}'"))
    }
}

/// Returns the file extension of `path`, or `None` for stdout (`-`) or a path
/// without one.
fn output_extension(path: &str) -> Option<&str> {
    if path == "-" {
        return None;
    }
    std::path::Path::new(path)
        .extension()
        .and_then(|ext| ext.to_str())
}

/// Reads `input`, detects its format (via `from` or probing), and parses it
/// into the internal [`Profile`] model.
fn load_profile(registry: &Registry, input: &str, from: Option<&str>) -> anyhow::Result<Profile> {
    let bytes = read_input(input)?;

    let format = if let Some(name) = from {
        registry
            .by_name(name)
            .with_context(|| format!("unknown input format '{name}'"))?
    } else {
        // Only use the path as a filename hint when it's a real file.
        let filename = (input != "-").then_some(input);
        let probe = ProbeData {
            filename,
            buf: &bytes,
        };
        let matched = registry
            .probe(&probe)
            .context("could not detect input format; specify it with --from")?;
        tracing::info!(
            format = matched.format.name(),
            confidence = ?matched.confidence,
            "auto-detected input format",
        );
        matched.format
    };

    format
        .read(&bytes)
        .with_context(|| format!("failed to parse input as '{}'", format.name()))
}

/// Picks the output format for `convert`: an explicit `--to` name wins,
/// otherwise it is inferred from the output path's extension, falling back to
/// JSON.
fn resolve_output<'a>(
    registry: &'a Registry,
    output: &str,
    to: Option<&str>,
) -> anyhow::Result<&'a dyn OutputFormat> {
    if let Some(name) = to {
        return registry
            .output_by_name(name)
            .with_context(|| format!("unknown output format '{name}'"));
    }

    let extension = output_extension(output);
    if let Some(format) = extension.and_then(|ext| registry.output_by_extension(ext)) {
        return Ok(format);
    }

    // No usable extension: fall back to JSON, but never silently - an
    // unrecognized extension (e.g. a misremembered `out.pb.gz`) almost always
    // means the user wanted a different format.
    if let Some(ext) = extension {
        tracing::warn!(
            extension = ext,
            "unrecognized output extension; defaulting to json. Use --to to choose a format.",
        );
    }
    registry
        .output_by_name("json")
        .context("default output format 'json' is not registered")
}

fn run_convert(
    registry: &Registry,
    input: &str,
    output: &str,
    from: Option<&str>,
    to: Option<&str>,
) -> anyhow::Result<()> {
    let profile = load_profile(registry, input, from)?;

    let format = resolve_output(registry, output, to)?;
    let encoded = format
        .write(&profile, WriteOptions { pretty: true })
        .with_context(|| format!("failed to encode profile as '{}'", format.name()))?;

    write_output(output, &encoded)?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn run_record(
    registry: &Registry,
    output: &str,
    pid: Option<u32>,
    self_target: bool,
    program: Option<&str>,
    freq: u32,
    duration: Option<f64>,
    source: Option<&str>,
    to: Option<&str>,
) -> anyhow::Result<()> {
    // clap enforces that at most one target is set; reject the empty case here.
    let target = match (pid, self_target, program) {
        (Some(pid), _, _) => Target::Pid(pid),
        (_, true, _) => Target::Current,
        (_, _, Some(command)) => {
            let argv = shlex::split(command)
                .filter(|argv| !argv.is_empty())
                .context("could not parse --program command line")?;
            Target::Command(argv)
        }
        (None, false, None) => {
            bail!("choose a target: --pid <PID>, --self, or --program \"<cmd>\"")
        }
    };

    let sources = Sources::with_builtins();
    let backend = match source {
        Some(name) => sources
            .by_name(name)
            .with_context(|| format!("unknown capture source '{name}'"))?,
        None => sources
            .available()
            .context("no capture backend is available on this platform")?,
    };
    if !backend.available() {
        bail!(
            "capture source '{}' is not available on this host (check permissions / platform support)",
            backend.name()
        );
    }

    let spec = CaptureSpec {
        target,
        frequency_hz: freq,
        duration: duration.map(Duration::from_secs_f64),
    };
    let profile = backend
        .capture(&spec)
        .with_context(|| format!("capture with '{}' failed", backend.name()))?;

    let format = resolve_output(registry, output, to)?;
    let encoded = format
        .write(&profile, WriteOptions { pretty: true })
        .with_context(|| format!("failed to encode profile as '{}'", format.name()))?;
    write_output(output, &encoded)?;
    Ok(())
}

fn run_dump(
    registry: &Registry,
    input: &str,
    from: Option<&str>,
    compact: bool,
) -> anyhow::Result<()> {
    let profile = load_profile(registry, input, from)?;

    let format = registry
        .output_by_name("json")
        .context("json output format is not registered")?;
    let mut encoded = format
        .write(&profile, WriteOptions { pretty: !compact })
        .context("failed to serialize profile")?;
    // Terminate with a newline so terminal output and pipes stay tidy.
    encoded.push(b'\n');

    write_output("-", &encoded)?;
    Ok(())
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    init_logging(cli.verbose);

    let registry = Registry::with_builtins();

    match cli.command {
        Command::Convert {
            input,
            output,
            from,
            to,
        } => run_convert(&registry, &input, &output, from.as_deref(), to.as_deref()),
        Command::Record {
            output,
            pid,
            self_target,
            program,
            freq,
            duration,
            source,
            to,
        } => run_record(
            &registry,
            &output,
            pid,
            self_target,
            program.as_deref(),
            freq,
            duration,
            source.as_deref(),
            to.as_deref(),
        ),
        Command::Dump {
            input,
            from,
            compact,
        } => run_dump(&registry, &input, from.as_deref(), compact),
    }
}
