//! Build script for `lean-rs-fuzz`.
//!
//! Bakes an rpath into the fuzz target binaries so they can load
//! `libleanshared.{dylib,so}` at run-time without `LD_LIBRARY_PATH` /
//! `DYLD_LIBRARY_PATH` games. Same rationale as
//! [`crates/lean-rs/build.rs`]: `lean-rs-sys`'s `cargo:rustc-link-arg`
//! rpath only applies to *its own* binaries, and the fuzz crate is a
//! separate sub-package (kept out of the workspace because libfuzzer
//! requires nightly Rust), so the rpath has to be re-emitted here for
//! the fuzz binaries that `cargo fuzz run` produces.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=LEAN_SYSROOT");
    println!("cargo:rerun-if-env-changed=PATH");

    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if !matches!(target_os.as_str(), "macos" | "linux") {
        return;
    }

    let Some(prefix) = discover_prefix() else {
        // Discovery failed — fall through silently. `lean-rs-sys` will
        // report the underlying problem with a more specific diagnostic
        // than this script could produce.
        return;
    };
    let lib_lean = prefix.join("lib").join("lean");
    println!("cargo:rustc-link-arg=-Wl,-rpath,{}", lib_lean.display());
}

fn discover_prefix() -> Option<PathBuf> {
    if let Some(p) = env::var_os("LEAN_SYSROOT") {
        return Some(PathBuf::from(p));
    }
    let output = Command::new("lean").arg("--print-prefix").output().ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8(output.stdout).ok()?;
    let trimmed = stdout.trim();
    if trimmed.is_empty() { None } else { Some(PathBuf::from(trimmed)) }
}
