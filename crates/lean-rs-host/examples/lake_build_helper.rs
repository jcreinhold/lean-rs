//! Smoke example for `lean_toolchain::build_lake_target`.
//!
//! This mirrors the downstream `build.rs` shape: ask `lean-toolchain` to build
//! a Lake shared-library target and return the dylib path without hand-written
//! Lake filename mangling. The helper emits `cargo:rerun-if-changed=...`
//! directives on stdout because build scripts communicate with Cargo through
//! stdout.

#![allow(clippy::expect_used, clippy::print_stdout)]

use std::path::PathBuf;

fn fixture_lake_root() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace = manifest_dir
        .parent()
        .and_then(std::path::Path::parent)
        .expect("crates/<name>/ lives two directories beneath the workspace root");
    workspace.join("fixtures").join("lean")
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dylib = lean_toolchain::build_lake_target(&fixture_lake_root(), "LeanRsFixture")?;
    println!("dylib={}", dylib.display());
    Ok(())
}
