#![allow(clippy::print_stdout)]

use lean_rs_profiling::report_schema::BaselineMode;
use lean_rs_profiling::report_writer::regenerate;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mode = mode_from_args();
    let markdown = regenerate(mode)?;
    println!("wrote {}", markdown.display());
    Ok(())
}

fn mode_from_args() -> BaselineMode {
    if std::env::args().any(|arg| arg == "--quick") {
        BaselineMode::Quick
    } else {
        BaselineMode::Full
    }
}
