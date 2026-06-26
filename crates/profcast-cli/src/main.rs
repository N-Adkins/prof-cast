#![allow(missing_docs)] // unimportant for cli structures

//! CLI for profcast

use std::io::{Read, Write};

use anyhow::{Context, bail};
use clap::{Parser, Subcommand};
use profcast_core::{format::ProbeData, model::Profile};
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

        /// Output format name. Currently only `json` (the internal model) is
        /// supported, which is also the default.
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

/// Reads `input`, detects its format (via `from` or probing), and parses it
/// into the internal [`Profile`] model.
fn load_profile(input: &str, from: Option<&str>) -> anyhow::Result<Profile> {
    let bytes = read_input(input)?;
    let registry = Registry::with_builtins();

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

fn run_convert(
    input: &str,
    output: &str,
    from: Option<&str>,
    to: Option<&str>,
) -> anyhow::Result<()> {
    let profile = load_profile(input, from)?;

    let to = to.unwrap_or("json");
    let encoded = match to {
        "json" => serde_json::to_vec_pretty(&profile).context("failed to serialize profile")?,
        other => bail!("unsupported output format '{other}' (only 'json' is supported)"),
    };

    write_output(output, &encoded)?;
    Ok(())
}

fn run_dump(input: &str, from: Option<&str>, compact: bool) -> anyhow::Result<()> {
    let profile = load_profile(input, from)?;

    let mut encoded = if compact {
        serde_json::to_vec(&profile)
    } else {
        serde_json::to_vec_pretty(&profile)
    }
    .context("failed to serialize profile")?;
    // Terminate with a newline so terminal output and pipes stay tidy.
    encoded.push(b'\n');

    write_output("-", &encoded)?;
    Ok(())
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    init_logging(cli.verbose);

    match cli.command {
        Command::Convert {
            input,
            output,
            from,
            to,
        } => run_convert(&input, &output, from.as_deref(), to.as_deref()),
        Command::Dump {
            input,
            from,
            compact,
        } => run_dump(&input, from.as_deref(), compact),
    }
}
