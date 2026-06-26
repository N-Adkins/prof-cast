#![allow(missing_docs)] // unimportant for cli structures

//! CLI for profcast

use clap::{Parser, Subcommand};
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
    Convert {
        input: String,
        output: String,

        #[arg(long)]
        from: Option<String>,

        #[arg(long)]
        to: Option<String>,
    },
}

fn init_logging(verbose: u8) {
    let default_level = match verbose {
        0 => "warn",
        1 => "info",
        2 => "debug",
        _ => "trace",
    };

    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(default_level));

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();
}

fn main() {
    let cli = Cli::parse();

    init_logging(cli.verbose);

    match cli.command {
        Command::Convert {
            input: _,
            output: _,
            from: _,
            to: _,
        } => {}
    }
}
