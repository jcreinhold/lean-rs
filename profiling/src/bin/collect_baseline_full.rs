#![allow(clippy::print_stdout)]

use lean_rs_profiling::report_schema::BaselineMode;
use lean_rs_profiling::runner::collect;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let (json, markdown) = collect(BaselineMode::Full)?;
    println!("wrote {}", json.display());
    println!("wrote {}", markdown.display());
    Ok(())
}
