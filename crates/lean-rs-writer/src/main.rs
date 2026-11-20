//! Standalone dispatch for the SDK shared artifact helper.
use std::process::ExitCode;

use clap::Parser;
use lean_rs_writer::{self as writer, ReadProxyArgs, ReaderArgs, WriterArgs, WriterLockPathArgs};

#[derive(Parser)]
#[command(version)]
enum Command {
    Writer(WriterArgs),
    WriterLockPath(WriterLockPathArgs),
    Reader(ReaderArgs),
    ReadProxy(ReadProxyArgs),
    #[command(hide = true)]
    WriterExec,
}

fn main() -> ExitCode {
    let action = Command::parse();
    let (name, result) = match &action {
        Command::Writer(args) => ("writer", writer::run_cli(args)),
        Command::WriterLockPath(args) => ("writer-lock-path", writer::run_lock_path_cli(args)),
        Command::Reader(args) => ("reader", writer::run_reader(args)),
        Command::ReadProxy(args) => ("read-proxy", writer::run_read_proxy(args)),
        Command::WriterExec => ("writer-exec", writer::run_exec()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("lean-rs-writer {name}: {error}");
            match action {
                Command::Reader(_) | Command::ReadProxy(_) => ExitCode::from(writer::reader_failure_exit_code(&error)),
                Command::Writer(_) | Command::WriterLockPath(_) | Command::WriterExec => ExitCode::FAILURE,
            }
        }
    }
}
