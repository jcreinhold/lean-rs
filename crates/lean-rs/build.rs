//! Build script for `lean-rs`.
//!
//! The only job here is to bake an rpath into this crate's test, bench,
//! example, and binary outputs so they can load `libleanshared.{dylib,so}`
//! at run-time without the developer having to set
//! `DYLD_FALLBACK_LIBRARY_PATH` / `LD_LIBRARY_PATH`.
//!
//! `lean-rs-sys`'s build script already emits an rpath for *its own*
//! binaries, but `cargo:rustc-link-arg` directives do not propagate to
//! dependents — each crate that produces an executable that loads Lean
//! must emit the flag itself.
//!
//! Toolchain discovery uses `lean --print-prefix` directly to avoid
//! pulling `lean-toolchain` into this crate's build-dependency graph for
//! a one-line probe.

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
        // Discovery failed — fall through silently. The link step in
        // `lean-rs-sys` will report the underlying problem with a more
        // specific diagnostic than this script could produce.
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
