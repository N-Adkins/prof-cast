#![allow(missing_docs)] // unimportant for cli structures

//! CLI for profcast

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "profcast")]
#[command(version = profcast_core::VERSION)]
struct Cli {
    #[command(subcommand)]
    command: Command,
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

fn main() {
    let cli = Cli::parse();

    match cli.command {
        Command::Convert {
            input: _,
            output: _,
            from: _,
            to: _,
        } => {}
    }
}
