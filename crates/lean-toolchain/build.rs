//! Build script for `lean-toolchain`.
//!
//! 1. Walk parents of `$CARGO_MANIFEST_DIR` until `fixtures/lean/lakefile.lean`
//!    is found; that directory roots the workspace Lake fixture.
//! 2. In a workspace checkout, compute a SHA-256 over a stable ordered
//!    concatenation of the fixture Lake manifest plus the compiled `.olean`
//!    and shared-library artifacts, when those artifacts already exist.
//!    Clean Cargo git checkouts and published crate tarballs may not contain
//!    `.lake` outputs; those builds record a zero digest instead.
//! 3. Write `$OUT_DIR/metadata.rs` with `LAKE_FIXTURE_DIGEST` and
//!    `HOST_TRIPLE` so `fingerprint.rs` can `include!` them.
//! 4. Emit `cargo:rerun-if-changed=*` for every input the digest depends on.
//!
//! This script emits no Lean runtime link or rpath directives. Link-free
//! toolchain metadata comes from `lean-rs-abi`; runtime link directives are
//! emitted only by crates that actually load Lean.

// Build scripts use `panic!` as the abort mechanism—same pattern as
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
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let docs_rs = env::var_os("DOCS_RS").is_some();
    let (digest, inputs) = find_fixture_dir(&manifest_dir).map_or_else(
        || {
            println!(
                "cargo:warning=lean-toolchain: workspace fixture not present in package build; LAKE_FIXTURE_DIGEST is zero"
            );
            ("0".repeat(64), Vec::new())
        },
        |fixture_dir| {
            if docs_rs {
                println!(
                    "cargo:warning=lean-toolchain: DOCS_RS=1; skipping workspace fixture digest and Lean runtime probes"
                );
                return ("0".repeat(64), Vec::new());
            }

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
                        "cargo:warning=lean-toolchain: missing fixture artifact {}; LAKE_FIXTURE_DIGEST is zero",
                        path.display()
                    );
                    return ("0".repeat(64), Vec::new());
                }
                let bytes = fs::read(path)
                    .unwrap_or_else(|err| panic!("lean-toolchain: cannot read {}: {err}", path.display()));
                // Domain-separated, length-prefixed: label, length, bytes. Prevents
                // boundary ambiguity between concatenated inputs.
                hasher.update((label.len() as u64).to_le_bytes());
                hasher.update(label.as_bytes());
                hasher.update((bytes.len() as u64).to_le_bytes());
                hasher.update(&bytes);
            }
            (hex(&hasher.finalize()), inputs)
        },
    );

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
    println!("cargo:rerun-if-env-changed=DOCS_RS");
}

fn find_fixture_dir(start: &Path) -> Option<PathBuf> {
    let mut cursor = start;
    loop {
        let candidate = cursor.join("fixtures").join("lean");
        if candidate.join("lakefile.lean").is_file() {
            return Some(candidate);
        }
        match cursor.parent() {
            Some(parent) => cursor = parent,
            None => return None,
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
