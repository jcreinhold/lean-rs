//! Standalone dispatch for the SDK shared artifact helper.
use std::process::ExitCode;

use clap::Parser;
use lean_rs_writer::HelperCommand;

#[derive(Parser)]
#[command(version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: HelperCommand,
}

fn main() -> ExitCode {
    Cli::parse().command.run("lean-rs-writer")
}
