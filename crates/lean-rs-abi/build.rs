//! Build script for `lean-rs-abi`.
//!
//! This script discovers the active Lean toolchain, verifies that its
//! `lean.h` belongs to the supported window, and bakes the resolved metadata
//! into Rust constants. It intentionally emits no linker directives; runtime
//! linkage belongs to `lean-rs-sys`.

// Build scripts panic to abort the build with a diagnostic. That is the
// correct failure mode here.
#![allow(clippy::manual_assert, clippy::panic)]

use sha2::{Digest, Sha256};
use std::env;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

#[path = "src/supported.rs"]
#[allow(dead_code, unreachable_pub)]
mod supported;

use supported::{SUPPORTED_TOOLCHAINS, SupportedToolchain};

fn main() {
    if env::var_os("DOCS_RS").is_some() {
        emit_docs_rs_metadata();
        return;
    }

    let prefix = discover_lean_prefix();
    println!(
        "cargo:warning=lean-rs-abi: using Lean toolchain prefix {}",
        prefix.display()
    );

    let header_path = prefix.join("include").join("lean").join("lean.h");
    let actual_digest = sha256_file(&header_path);
    let entry = SUPPORTED_TOOLCHAINS
        .iter()
        .find(|t| t.header_digest == actual_digest)
        .unwrap_or_else(|| {
            panic!(
                "lean-rs-abi: lean.h at {} has digest {} but no entry in \
                 SUPPORTED_TOOLCHAINS matches. The supported window is:\n  - {}\n\
                 Either install a supported Lean toolchain, or follow \
                 docs/bump-toolchain.md to add this one.",
                header_path.display(),
                actual_digest,
                window_summary(),
            )
        });

    let discovered_version = read_lean_version(&prefix);
    let resolved_version = pick_resolved_version(entry, &discovered_version);

    println!("cargo:rustc-env=LEAN_VERSION={discovered_version}");
    println!("cargo:rustc-env=LEAN_RESOLVED_VERSION={resolved_version}");
    println!("cargo:rustc-env=LEAN_HEADER_PATH={}", header_path.display());
    println!("cargo:rustc-env=LEAN_HEADER_DIGEST={actual_digest}");
    emit_version_cfgs(resolved_version);
    emit_rerun_triggers(Some(&header_path));
}

fn emit_docs_rs_metadata() {
    let entry = SUPPORTED_TOOLCHAINS
        .last()
        .unwrap_or_else(|| panic!("lean-rs-abi: SUPPORTED_TOOLCHAINS is empty"));
    let resolved_version = entry
        .versions
        .first()
        .copied()
        .unwrap_or_else(|| panic!("lean-rs-abi: latest supported toolchain entry has no versions"));

    println!("cargo:warning=lean-rs-abi: DOCS_RS=1; emitting documentation metadata without probing Lean");
    println!("cargo:rustc-env=LEAN_VERSION={resolved_version}");
    println!("cargo:rustc-env=LEAN_RESOLVED_VERSION={resolved_version}");
    println!("cargo:rustc-env=LEAN_HEADER_PATH=<docs.rs synthetic lean.h>");
    println!("cargo:rustc-env=LEAN_HEADER_DIGEST={}", entry.header_digest);
    emit_version_cfgs(resolved_version);
    emit_rerun_triggers(None);
}

fn pick_resolved_version<'a>(entry: &'a SupportedToolchain, discovered: &'a str) -> &'a str {
    if entry.versions.contains(&discovered) {
        return discovered;
    }
    entry.versions.first().copied().unwrap_or("unknown")
}

fn cfg_token(version: &str) -> String {
    version
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' { c } else { '_' })
        .collect()
}

fn minor_key(version: &str) -> Option<(u32, u32)> {
    let numeric = version.split(['-', '+']).next().unwrap_or(version);
    let mut parts = numeric.split('.');
    let major: u32 = parts.next()?.parse().ok()?;
    let minor: u32 = parts.next()?.parse().ok()?;
    Some((major, minor))
}

fn emit_version_cfgs(resolved_version: &str) {
    use std::collections::BTreeSet;

    let mut version_tokens: BTreeSet<String> = BTreeSet::new();
    let mut minors: BTreeSet<(u32, u32)> = BTreeSet::new();
    for t in SUPPORTED_TOOLCHAINS {
        for v in t.versions {
            version_tokens.insert(cfg_token(v));
            if let Some(key) = minor_key(v) {
                minors.insert(key);
            }
        }
    }

    for token in &version_tokens {
        println!("cargo:rustc-check-cfg=cfg(lean_v_{token})");
    }
    for (major, minor) in &minors {
        println!("cargo:rustc-check-cfg=cfg(lean_at_least_{major}_{minor})");
    }

    println!("cargo:rustc-cfg=lean_v_{}", cfg_token(resolved_version));
    if let Some(resolved_key) = minor_key(resolved_version) {
        for &(major, minor) in &minors {
            if (major, minor) <= resolved_key {
                println!("cargo:rustc-cfg=lean_at_least_{major}_{minor}");
            }
        }
    }
}

fn emit_rerun_triggers(header_path: Option<&Path>) {
    println!("cargo:rerun-if-env-changed=LEAN_SYSROOT");
    println!("cargo:rerun-if-env-changed=ELAN_HOME");
    println!("cargo:rerun-if-env-changed=PATH");
    println!("cargo:rerun-if-env-changed=DOCS_RS");
    if let Some(header_path) = header_path {
        println!("cargo:rerun-if-changed={}", header_path.display());
    }
    println!("cargo:rerun-if-changed=src/supported.rs");
    println!("cargo:rerun-if-changed=src/symbols.rs");
    println!("cargo:rerun-if-changed=build.rs");
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

fn discover_lean_prefix() -> PathBuf {
    let mut tried: Vec<String> = Vec::new();

    if let Some(sysroot) = env::var_os("LEAN_SYSROOT") {
        let p = PathBuf::from(&sysroot);
        if p.join("include/lean/lean.h").is_file() {
            return p;
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
                return p;
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
                        return p;
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

    if let Some(p) = fixture_prefix() {
        if p.join("include/lean/lean.h").is_file() {
            return p;
        }
        tried.push(format!("fixture prefix {} (no include/lean/lean.h)", p.display()));
    } else {
        tried.push("fixture prefix: `lake env printenv LEAN_SYSROOT` unavailable".into());
    }

    panic!(
        "lean-rs-abi: could not locate a Lean toolchain prefix. Tried:\n  - {}",
        tried.join("\n  - ")
    );
}

fn fixture_prefix() -> Option<PathBuf> {
    let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR")?);
    let mut cursor = manifest.as_path();
    let fixture_dir = loop {
        let candidate = cursor.join("fixtures").join("lean");
        if candidate.join("lakefile.lean").is_file() || candidate.join("lakefile.toml").is_file() {
            break candidate;
        }
        cursor = cursor.parent()?;
    };

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
    let bytes = fs::read(path).unwrap_or_else(|err| panic!("lean-rs-abi: cannot read {}: {err}", path.display()));
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    let digest = hasher.finalize();
    let mut out = String::with_capacity(digest.len().saturating_mul(2));
    for byte in digest {
        let _ = write!(out, "{byte:02x}");
    }
    out
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
