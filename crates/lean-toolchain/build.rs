//! Build script for `lean-toolchain`.
//!
//! 1. Walk parents of `$CARGO_MANIFEST_DIR` until `fixtures/lean/lakefile.lean`
//!    is found; that directory roots the Lake fixture.
//! 2. Compute a SHA-256 over a stable ordered concatenation of the fixture
//!    Lake manifest plus the compiled `.olean` and shared-library artifacts.
//!    If any artifact is absent, abort with a `cargo:warning=` line naming
//!    the path and the recovery command.
//! 3. Write `$OUT_DIR/metadata.rs` with `LAKE_FIXTURE_DIGEST` and
//!    `HOST_TRIPLE` so `fingerprint.rs` can `include!` them.
//! 4. Emit `cargo:rerun-if-changed=*` for every input the digest depends on
//!    and `cargo:rerun-if-env-changed=*` for every env var `discover.rs`
//!    consults at runtime.
//!
//! This script does not emit `cargo:rustc-link-search` or
//! `cargo:rustc-link-lib` directives — `lean-rs-sys` already does that
//! for the whole dependency graph, and `emit_lean_link_directives()` in
//! `src/build_helpers.rs` is the helper downstream embedders call from
//! their own `build.rs`. It does emit a `cargo:rustc-link-arg=-Wl,-rpath,...`
//! because `link-arg` directives do not propagate from `lean-rs-sys` to
//! dependents, so each crate that produces a test/bench/example binary
//! that loads Lean must bake its own rpath.

// Build scripts use `panic!` as the abort mechanism — same pattern as
// `lean-rs-sys/build.rs`.
#![allow(clippy::expect_used, clippy::manual_assert, clippy::panic, clippy::unwrap_used)]

use sha2::{Digest, Sha256};
use std::env;
use std::fmt::Write as _;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

fn main() {
    let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR unset"));
    let fixture_dir = find_fixture_dir(&manifest_dir);
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let dylib_ext = match target_os.as_str() {
        "macos" => "dylib",
        "linux" => "so",
        other => panic!("lean-toolchain: unsupported target_os `{other}`; only macos and linux are tested"),
    };

    // Lake's shared-library filename changed between Lean 4.26 and 4.27:
    // older versions emit `liblean_rs_fixture.{dylib,so}` (just the lib
    // name); 4.27+ emit `liblean__rs__fixture_LeanRsFixture.{dylib,so}`
    // (package-escaped + lib name). Probe both candidates so the build
    // works across the supported window.
    let lib_dir = fixture_dir.join(".lake/build/lib");
    let new_style = lib_dir.join(format!("liblean__rs__fixture_LeanRsFixture.{dylib_ext}"));
    let old_style = lib_dir.join(format!("libLeanRsFixture.{dylib_ext}"));
    let fixture_dylib = if new_style.is_file() {
        (new_style, "liblean__rs__fixture_LeanRsFixture")
    } else {
        (old_style, "libLeanRsFixture")
    };
    let inputs: Vec<(PathBuf, &str)> = vec![
        (fixture_dir.join("lakefile.lean"), "lakefile.lean"),
        (fixture_dir.join("lake-manifest.json"), "lake-manifest.json"),
        (
            fixture_dir.join(".lake/build/lib/lean/LeanRsFixture.olean"),
            "LeanRsFixture.olean",
        ),
        fixture_dylib,
    ];

    let mut hasher = Sha256::new();
    for (path, label) in &inputs {
        if !path.is_file() {
            println!(
                "cargo:warning=lean-toolchain: missing fixture artifact {} (run `cd fixtures/lean && lake build`)",
                path.display()
            );
            panic!(
                "lean-toolchain: missing fixture artifact {} ({label}); run `cd fixtures/lean && lake build`",
                path.display()
            );
        }
        let bytes =
            fs::read(path).unwrap_or_else(|err| panic!("lean-toolchain: cannot read {}: {err}", path.display()));
        // Domain-separated, length-prefixed: label, length, bytes. Prevents
        // boundary ambiguity between concatenated inputs.
        hasher.update((label.len() as u64).to_le_bytes());
        hasher.update(label.as_bytes());
        hasher.update((bytes.len() as u64).to_le_bytes());
        hasher.update(&bytes);
    }
    let digest = hex(&hasher.finalize());

    let host_triple = env::var("TARGET").unwrap_or_else(|_| "unknown".to_string());

    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR unset"));
    let metadata_path = out_dir.join("metadata.rs");
    let mut file = fs::File::create(&metadata_path)
        .unwrap_or_else(|err| panic!("lean-toolchain: cannot create {}: {err}", metadata_path.display()));
    writeln!(
        file,
        "/// SHA-256 over the workspace Lake fixture's manifest plus compiled native artifacts.\n\
         pub const LAKE_FIXTURE_DIGEST: &str = \"{digest}\";\n\
         /// Cargo `TARGET` triple `lean-toolchain` was built for.\n\
         pub const HOST_TRIPLE: &str = \"{host_triple}\";"
    )
    .unwrap_or_else(|err| panic!("lean-toolchain: cannot write {}: {err}", metadata_path.display()));

    for (path, _) in &inputs {
        println!("cargo:rerun-if-changed={}", path.display());
    }
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=LEAN_SYSROOT");
    println!("cargo:rerun-if-env-changed=ELAN_HOME");
    println!("cargo:rerun-if-env-changed=PATH");

    // Bake an rpath into this crate's test binaries so they can load
    // `libleanshared.{dylib,so}` at run-time. `lean-toolchain` itself
    // does not call into Lean, but it depends on `lean-rs-sys` whose
    // build script attaches `libleanshared` to the link line; the test
    // binary therefore needs to be able to resolve the dylib at load
    // time. `cargo:rustc-link-arg` directives do not propagate from
    // `lean-rs-sys` to dependents, so each crate that produces an
    // executable that loads Lean emits the rpath itself.
    if matches!(target_os.as_str(), "macos" | "linux")
        && let Some(prefix) = discover_prefix()
    {
        let lib_lean = prefix.join("lib").join("lean");
        println!("cargo:rustc-link-arg=-Wl,-rpath,{}", lib_lean.display());
    }
}

fn discover_prefix() -> Option<PathBuf> {
    if let Some(p) = env::var_os("LEAN_SYSROOT") {
        return Some(PathBuf::from(p));
    }
    let output = std::process::Command::new("lean").arg("--print-prefix").output().ok()?;
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

fn find_fixture_dir(start: &Path) -> PathBuf {
    let mut cursor = start;
    loop {
        let candidate = cursor.join("fixtures").join("lean");
        if candidate.join("lakefile.lean").is_file() {
            return candidate;
        }
        match cursor.parent() {
            Some(parent) => cursor = parent,
            None => panic!(
                "lean-toolchain: could not find `fixtures/lean/lakefile.lean` walking up from {}",
                start.display()
            ),
        }
    }
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        let _ = write!(out, "{byte:02x}");
    }
    out
}
