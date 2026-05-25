//! Build script for `lean-rs-worker-protocol`.
//!
//! The crate's own source code does not call into Lean; downstream consumers
//! depend on it via `lean-toolchain` with `lean-rs-sys`'s `metadata-only`
//! feature, so a published consumer build never links `libleanshared`.
//!
//! Inside this workspace, however, Cargo's feature unification pulls
//! `lean-rs-sys/dynamic` into every test binary via the sibling `lean-rs` and
//! `lean-rs-host` crates. That makes `libleanshared.{dylib,so}` a runtime
//! load dependency of this crate's test binary even though the test code
//! never resolves a Lean symbol. `cargo:rustc-link-arg` directives do not
//! propagate from `lean-rs-sys`, so this script bakes an rpath into the test
//! binary so workspace-wide `cargo nextest run` can load the dylib.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=DOCS_RS");
    println!("cargo:rerun-if-env-changed=LEAN_SYSROOT");
    println!("cargo:rerun-if-env-changed=PATH");

    if env::var_os("DOCS_RS").is_some() {
        return;
    }

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
    let trimmed = String::from_utf8(output.stdout).ok()?.trim().to_owned();
    if trimmed.is_empty() {
        None
    } else {
        Some(PathBuf::from(trimmed))
    }
}
