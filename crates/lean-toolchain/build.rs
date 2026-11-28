//! Build script for `lean-toolchain`.
//!
//! 1. Resolve the active Lean toolchain's identity—the installed
//!    `LEAN_VERSION_STRING`, the resolved supported-window version, the
//!    `lean.h` path, and its SHA-256—by probing `LEAN_SYSROOT`, `lean`,
//!    `elan`, and the workspace Lake fixture. When no toolchain is present
//!    this **degrades** to the latest supported window entry instead of
//!    failing, so link-free downstream crates (and docs.rs) build with no Lean
//!    installed. The crate that *hard*-requires a toolchain is `lean-rs-sys`,
//!    because it links `libleanshared`; a link-free metadata/discovery crate
//!    must not.
//! 2. Write `$OUT_DIR/metadata.rs` with the toolchain identity constants plus
//!    `HOST_TRIPLE` so `fingerprint.rs` can `include!` them.
//! 3. Emit `cargo:rerun-if-*` for every input the metadata depends on.
//!
//! The script reads no Lake output. Its last fallback for the Lean prefix asks
//! Lake, through `lake env`, in the workspace's `fixtures/lean` project.
//!
//! This script emits no Lean runtime link or rpath directives. Runtime link
//! directives are emitted only by crates that actually load Lean.

// Build scripts use `panic!` as the abort mechanism—same pattern as
// `lean-rs-sys/build.rs`. The only abort here is an *installed* toolchain whose
// `lean.h` is outside the supported window; a *missing* toolchain degrades.
#![allow(clippy::expect_used, clippy::manual_assert, clippy::panic, clippy::unwrap_used)]

use lean_rs_abi::supported::{SUPPORTED_TOOLCHAINS, SupportedToolchain};
use sha2::{Digest, Sha256};
use std::env;
use std::fmt::Write as _;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Build-time identity of the Lean toolchain this crate is compiled against,
/// or the latest supported window entry on a metadata-only build.
struct ToolchainMetadata {
    lean_version: String,
    resolved_version: String,
    header_path: String,
    header_digest: String,
}

fn main() {
    let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR unset"));
    let docs_rs = env::var_os("DOCS_RS").is_some();

    let toolchain = resolve_toolchain_metadata(&manifest_dir, docs_rs);

    let host_triple = env::var("TARGET").unwrap_or_else(|_| "unknown".to_string());

    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR unset"));
    let metadata_path = out_dir.join("metadata.rs");
    let mut file = fs::File::create(&metadata_path)
        .unwrap_or_else(|err| panic!("lean-toolchain: cannot create {}: {err}", metadata_path.display()));
    writeln!(
        file,
        "/// `LEAN_VERSION_STRING` read from the active toolchain's `version.h`, or the\n\
         /// latest supported version on a metadata-only build with no toolchain present.\n\
         pub const LEAN_VERSION: &str = {lean_version:?};\n\
         /// Version string from the matched supported-toolchain entry (equal to\n\
         /// `LEAN_VERSION` except when several releases share one `lean.h`).\n\
         pub const LEAN_RESOLVED_VERSION: &str = {resolved_version:?};\n\
         /// Filesystem path to the `lean.h` this build resolved against, or a sentinel\n\
         /// on a metadata-only build with no toolchain present.\n\
         pub const LEAN_HEADER_PATH: &str = {header_path:?};\n\
         /// SHA-256 of the resolved `lean.h`, lowercase hex.\n\
         pub const LEAN_HEADER_DIGEST: &str = {header_digest:?};\n\
         /// Cargo `TARGET` triple `lean-toolchain` was built for.\n\
         pub(crate) const HOST_TRIPLE: &str = \"{host_triple}\";",
        lean_version = toolchain.lean_version,
        resolved_version = toolchain.resolved_version,
        header_path = toolchain.header_path,
        header_digest = toolchain.header_digest,
    )
    .unwrap_or_else(|err| panic!("lean-toolchain: cannot write {}: {err}", metadata_path.display()));

    if Path::new(&toolchain.header_path).is_file() {
        println!("cargo:rerun-if-changed={}", toolchain.header_path);
    }
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=LEAN_SYSROOT");
    println!("cargo:rerun-if-env-changed=ELAN_HOME");
    println!("cargo:rerun-if-env-changed=PATH");
    println!("cargo:rerun-if-env-changed=DOCS_RS");
}

/// Resolve the live toolchain identity, degrading to the latest supported
/// window entry when no toolchain is present (or under `DOCS_RS`). Aborts only
/// when an *installed* toolchain's `lean.h` is outside the supported window.
fn resolve_toolchain_metadata(manifest_dir: &Path, docs_rs: bool) -> ToolchainMetadata {
    if docs_rs {
        println!("cargo:warning=lean-toolchain: DOCS_RS=1; emitting latest-supported Lean metadata without probing");
        return latest_supported_metadata();
    }
    let prefix = match discover_lean_prefix(manifest_dir) {
        Ok(prefix) => prefix,
        Err(tried) => {
            println!(
                "cargo:warning=lean-toolchain: no Lean toolchain found; emitting latest-supported metadata \
                 (metadata-only build). Tried:\n  - {tried}"
            );
            return latest_supported_metadata();
        }
    };

    let header_path = prefix.join("include").join("lean").join("lean.h");
    let actual_digest = sha256_file(&header_path);
    let entry = SUPPORTED_TOOLCHAINS
        .iter()
        .find(|t| t.header_digest == actual_digest)
        .unwrap_or_else(|| {
            panic!(
                "lean-toolchain: lean.h at {} has digest {} but no entry in \
                 SUPPORTED_TOOLCHAINS matches. The supported window is:\n  - {}\n\
                 Either install a supported Lean toolchain, or follow \
                 docs/bump-toolchain.md to add this one.",
                header_path.display(),
                actual_digest,
                window_summary(),
            )
        });

    let discovered_version = read_lean_version(&prefix);
    let resolved_version = pick_resolved_version(entry, &discovered_version).to_string();
    ToolchainMetadata {
        lean_version: discovered_version,
        resolved_version,
        header_path: header_path.display().to_string(),
        header_digest: actual_digest,
    }
}

/// Static metadata for the newest supported toolchain, used when no toolchain
/// is installed. The header path is a sentinel: there is no `lean.h` on disk.
fn latest_supported_metadata() -> ToolchainMetadata {
    let entry = SUPPORTED_TOOLCHAINS
        .last()
        .unwrap_or_else(|| panic!("lean-toolchain: SUPPORTED_TOOLCHAINS is empty"));
    let version = entry
        .versions
        .first()
        .copied()
        .unwrap_or_else(|| panic!("lean-toolchain: latest supported toolchain entry has no versions"));
    ToolchainMetadata {
        lean_version: version.to_string(),
        resolved_version: version.to_string(),
        header_path: "<no Lean toolchain; metadata-only build>".to_string(),
        header_digest: entry.header_digest.to_string(),
    }
}

fn pick_resolved_version<'a>(entry: &'a SupportedToolchain, discovered: &'a str) -> &'a str {
    if entry.versions.contains(&discovered) {
        return discovered;
    }
    entry.versions.first().copied().unwrap_or("unknown")
}

fn window_summary() -> String {
    let mut out = String::new();
    for (i, t) in SUPPORTED_TOOLCHAINS.iter().enumerate() {
        if i > 0 {
            out.push_str("\n  - ");
        }
        let _ = write!(out, "{:?} (digest {})", t.versions, t.header_digest);
    }
    out
}

/// Locate the active Lean prefix, trying `LEAN_SYSROOT`, `lean --print-prefix`,
/// `$ELAN_HOME` + `elan show active-toolchain`, then the workspace Lake
/// fixture. Returns the tried-location summary on failure instead of aborting.
fn discover_lean_prefix(manifest_dir: &Path) -> Result<PathBuf, String> {
    let mut tried: Vec<String> = Vec::new();

    if let Some(sysroot) = env::var_os("LEAN_SYSROOT") {
        let p = PathBuf::from(&sysroot);
        if p.join("include/lean/lean.h").is_file() {
            return Ok(p);
        }
        tried.push(format!("LEAN_SYSROOT={} (no include/lean/lean.h)", p.display()));
    } else {
        tried.push("LEAN_SYSROOT unset".into());
    }

    match Command::new("lean").arg("--print-prefix").output() {
        Ok(out) if out.status.success() => {
            let prefix = String::from_utf8_lossy(&out.stdout).trim().to_string();
            let p = PathBuf::from(&prefix);
            if p.join("include/lean/lean.h").is_file() {
                return Ok(p);
            }
            tried.push(format!("`lean --print-prefix` = {prefix} (no include/lean/lean.h)"));
        }
        Ok(out) => tried.push(format!(
            "`lean --print-prefix` exited {} (stderr: {})",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        )),
        Err(err) => tried.push(format!("`lean --print-prefix` failed: {err}")),
    }

    if let Some(elan_home) = env::var_os("ELAN_HOME") {
        let elan = PathBuf::from(&elan_home);
        match Command::new("elan").arg("show").arg("active-toolchain").output() {
            Ok(out) if out.status.success() => {
                let line = String::from_utf8_lossy(&out.stdout)
                    .lines()
                    .next()
                    .unwrap_or("")
                    .split_whitespace()
                    .next()
                    .unwrap_or("")
                    .to_string();
                if !line.is_empty() {
                    let p = elan.join("toolchains").join(&line);
                    if p.join("include/lean/lean.h").is_file() {
                        return Ok(p);
                    }
                    tried.push(format!("$ELAN_HOME/toolchains/{line} (no include/lean/lean.h)"));
                }
            }
            Ok(out) => tried.push(format!("`elan show active-toolchain` exited {}", out.status)),
            Err(err) => tried.push(format!("`elan show active-toolchain` failed: {err}")),
        }
    } else {
        tried.push("ELAN_HOME unset".into());
    }

    if let Some(p) = fixture_prefix(manifest_dir) {
        if p.join("include/lean/lean.h").is_file() {
            return Ok(p);
        }
        tried.push(format!("fixture prefix {} (no include/lean/lean.h)", p.display()));
    } else {
        tried.push("fixture prefix: `lake env printenv LEAN_SYSROOT` unavailable".into());
    }

    Err(tried.join("\n  - "))
}

fn fixture_prefix(manifest_dir: &Path) -> Option<PathBuf> {
    let fixture_dir = find_fixture_dir(manifest_dir)?;
    let out = Command::new("lake")
        .arg("env")
        .arg("printenv")
        .arg("LEAN_SYSROOT")
        .current_dir(&fixture_dir)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let prefix = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if prefix.is_empty() {
        return None;
    }
    Some(PathBuf::from(prefix))
}

fn sha256_file(path: &Path) -> String {
    let bytes = fs::read(path).unwrap_or_else(|err| panic!("lean-toolchain: cannot read {}: {err}", path.display()));
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    hex(&hasher.finalize())
}

fn read_lean_version(prefix: &Path) -> String {
    let version_h = prefix.join("include").join("lean").join("version.h");
    if let Ok(text) = fs::read_to_string(&version_h) {
        for line in text.lines() {
            if let Some(rest) = line.trim().strip_prefix("#define LEAN_VERSION_STRING") {
                let trimmed = rest.trim().trim_matches('"');
                if !trimmed.is_empty() {
                    return trimmed.to_string();
                }
            }
        }
    }
    match Command::new("lean").arg("--version").output() {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout)
            .split_whitespace()
            .nth(3)
            .unwrap_or("unknown")
            .to_string(),
        _ => "unknown".to_string(),
    }
}

fn find_fixture_dir(start: &Path) -> Option<PathBuf> {
    let mut cursor = start;
    loop {
        let candidate = cursor.join("fixtures").join("lean");
        if candidate.join("lakefile.lean").is_file() {
            return Some(candidate);
        }
        cursor = cursor.parent()?;
    }
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        let _ = write!(out, "{byte:02x}");
    }
    out
}
