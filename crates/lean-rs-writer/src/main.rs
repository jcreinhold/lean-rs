//! Standalone dispatch for the SDK shared artifact helper.
use std::io::{self, Write as _};
use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use lean_rs_writer::{self as writer, ReadProxyArgs, ReaderArgs, WriterArgs, WriterLockPathArgs};
use lean_toolchain::native_guardian::writer_storage::StorageRoot;

#[derive(Parser)]
#[command(version)]
enum Command {
    Writer(WriterArgs),
    WriterLockPath(WriterLockPathArgs),
    /// Inspect or settle one exact writer storage turn.
    #[command(subcommand)]
    WriterStorage(StorageCommand),
    Reader(ReaderArgs),
    ReadProxy(ReadProxyArgs),
    #[command(hide = true)]
    WriterExec,
}

#[derive(clap::Args)]
struct Selection {
    /// Independently selected canonical registry directory.
    #[arg(long)]
    registry: PathBuf,
    #[arg(long)]
    registry_device: u64,
    #[arg(long)]
    registry_inode: u64,
    /// Original 32-digit hexadecimal storage turn.
    #[arg(long, value_parser = parse_turn)]
    turn: String,
}

fn parse_turn(value: &str) -> Result<String, &'static str> {
    if value.len() == 32 && value.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()) {
        Ok(value.to_owned())
    } else {
        Err("expected the original 32 lowercase hexadecimal digits")
    }
}

#[derive(clap::Subcommand)]
enum StorageCommand {
    /// Preview the reservation and any missing evidence without changing it.
    Inspect(Selection),
    /// Recheck Idle custody, clean the bound scratch and settle once.
    Recover {
        #[command(flatten)]
        selection: Selection,
        /// Acknowledge that settlement leaves the interrupted work result unknown.
        #[arg(long, required = true)]
        acknowledge_unknown_result: bool,
    },
}

impl StorageCommand {
    fn run(&self) -> io::Result<()> {
        let (selection, recover) = match self {
            Self::Inspect(selection) => (selection, false),
            Self::Recover {
                selection,
                acknowledge_unknown_result: true,
            } => (selection, true),
            Self::Recover { .. } => return Err(io::Error::other("acknowledge the unknown result before recovery")),
        };
        let registry = StorageRoot {
            path: selection.registry.clone(),
            device: selection.registry_device,
            inode: selection.registry_inode,
        };
        let (report, ok) = if recover {
            writer::recover_idle_writer_storage_turn(&registry, &selection.turn)
        } else {
            let report = writer::inspect_writer_storage_turn(&registry, &selection.turn);
            let ok = report.get("state").and_then(serde_json::Value::as_str) == Some("eligible");
            (report, ok)
        };
        let mut output = io::stdout().lock();
        serde_json::to_writer(&mut output, &report)?;
        writeln!(output)?;
        output.flush()?;
        if !ok {
            return Err(io::Error::other(
                report
                    .get("reason")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("turn unavailable"),
            ));
        }
        Ok(())
    }
}

fn main() -> ExitCode {
    let action = Command::parse();
    let (name, result) = match &action {
        Command::Writer(args) => ("writer", writer::run_cli(args)),
        Command::WriterLockPath(args) => ("writer-lock-path", writer::run_lock_path_cli(args)),
        Command::WriterStorage(command) => ("writer-storage", command.run()),
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
                Command::Writer(_) | Command::WriterLockPath(_) | Command::WriterStorage(_) | Command::WriterExec => {
                    ExitCode::FAILURE
                }
            }
        }
    }
}
