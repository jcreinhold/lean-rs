//! Build script for `lean-rs-worker`.
//!
//! The worker crate produces a child binary and integration-test binaries
//! that link Lean. `lean-rs-sys` emits link directives for its own outputs,
//! but `cargo:rustc-link-arg` rpath directives do not propagate to
//! dependents, so this crate bakes the active Lean runtime path into its
//! executables too.

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
    if trimmed.is_empty() {
        None
    } else {
        Some(PathBuf::from(trimmed))
    }
}
