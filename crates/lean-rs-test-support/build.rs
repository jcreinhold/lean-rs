//! Build script for `lean-rs-test-support`.
//!
//! TODO(prompt 12): remove. This script exists solely to make the
//! workspace-internal `fixture` module callable from `lean-rs`'s tests.
//! Prompts 11 + 12's `LeanLibrary` + `LeanModule` + `LeanExported{N}`
//! machinery loads compiled Lean modules through the safe surface, at
//! which point this script and the `fixture` module below become
//! unnecessary.
//!
//! Until then we:
//!
//! 1. Locate the Lake-built fixture artifacts under
//!    `<workspace>/fixtures/lean/.lake/build/lib/`. The path is derived
//!    statically from `CARGO_MANIFEST_DIR` — no toolchain probing needed.
//! 2. Emit `cargo:rustc-link-search=native=...` and
//!    `cargo:rustc-link-lib=dylib=lean__rs__fixture_LeanRsFixture` so the
//!    `extern "C"` declarations in `src/lib.rs` resolve at link time.
//! 3. Bake an rpath into our own test binaries so the dylib resolves at
//!    run time. This mirrors the pattern in `lean-rs/build.rs` and
//!    `lean-rs-sys/build.rs`: `cargo:rustc-link-arg` directives do not
//!    propagate, so each crate that loads Lean must emit its own.
//!
//! On a fresh clone where the fixture has never been built, the script
//! aborts with a diagnostic naming the missing path and the recovery
//! command (`cd fixtures/lean && lake build`).

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=LEAN_SYSROOT");
    println!("cargo:rerun-if-env-changed=PATH");

    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR set by cargo"));
    let workspace_root = manifest_dir
        .parent()
        .and_then(std::path::Path::parent)
        .expect("crates/<name>/ lives two directories beneath the workspace root");
    let fixture_lib_dir = workspace_root
        .join("fixtures")
        .join("lean")
        .join(".lake")
        .join("build")
        .join("lib");
    let dylib_extension = if cfg!(target_os = "macos") { "dylib" } else { "so" };
    let dylib_path = fixture_lib_dir.join(format!("liblean__rs__fixture_LeanRsFixture.{dylib_extension}"));
    assert!(
        dylib_path.exists(),
        "lean-rs-test-support: fixture dylib not found at {} — run `cd fixtures/lean && lake build` to produce it",
        dylib_path.display()
    );

    println!("cargo:rustc-link-search=native={}", fixture_lib_dir.display());
    println!("cargo:rustc-link-lib=dylib=lean__rs__fixture_LeanRsFixture");
    println!("cargo:rerun-if-changed={}", dylib_path.display());

    // rpaths: the test binary may load both the fixture dylib (in
    // `fixture_lib_dir`) and `libleanshared` (under `<lean-prefix>/lib/lean`).
    println!("cargo:rustc-link-arg=-Wl,-rpath,{}", fixture_lib_dir.display());
    if let Some(prefix) = discover_lean_prefix() {
        let lib_lean = prefix.join("lib").join("lean");
        println!("cargo:rustc-link-arg=-Wl,-rpath,{}", lib_lean.display());
    }
}

fn discover_lean_prefix() -> Option<PathBuf> {
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
