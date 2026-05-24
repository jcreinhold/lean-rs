//! Typed Lean toolchain discovery.
//!
//! Probe precedence (first probe whose `<prefix>/include/lean/lean.h` exists wins):
//!
//! 1. `DiscoverOptions::explicit_sysroot`.
//! 2. `$LEAN_SYSROOT`.
//! 3. `$ELAN_HOME` + `elan show active-toolchain`.
//! 4. `lean --print-prefix` (via `PATH`).
//! 5. `lake env printenv LEAN_SYSROOT` under a workspace `fixtures/lean` directory.
//!
//! Each probe is independently gated by a `DiscoverOptions` flag so callers
//! can lock down behaviour for reproducible builds.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::diagnostics::LinkDiagnostics;
use crate::fingerprint::ToolchainFingerprint;

/// Which probe produced the resolved toolchain.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiscoverySource {
    /// Caller supplied `DiscoverOptions::explicit_sysroot`.
    ExplicitSysroot,
    /// Caller had `LEAN_SYSROOT` exported in the environment.
    LeanSysrootEnv,
    /// Resolved via `$ELAN_HOME` + `elan show active-toolchain`.
    ElanHome,
    /// Resolved via `lean --print-prefix` on `PATH`.
    Path,
    /// Resolved via `lake env printenv LEAN_SYSROOT` under a workspace fixture.
    LakeFixtureEnv,
}

/// Knobs that gate which discovery probes run.
///
/// `Default` enables every probe; tests and reproducible builds narrow the
/// set to produce deterministic behaviour.
#[derive(Clone, Debug)]
pub struct DiscoverOptions {
    /// Caller-supplied sysroot; bypasses every other probe when set.
    pub explicit_sysroot: Option<PathBuf>,
    /// Consult `lean --print-prefix` (requires `lean` on `PATH`).
    pub allow_path_lookup: bool,
    /// Consult `$ELAN_HOME` + `elan show active-toolchain`.
    pub allow_elan: bool,
    /// Consult `lake env printenv LEAN_SYSROOT` under `fixtures/lean`.
    pub allow_lake_env: bool,
    /// Optional `lean-toolchain` file to parse for the recorded version.
    pub toolchain_file: Option<PathBuf>,
}

impl Default for DiscoverOptions {
    fn default() -> Self {
        Self {
            explicit_sysroot: None,
            allow_path_lookup: true,
            allow_elan: true,
            allow_lake_env: true,
            toolchain_file: None,
        }
    }
}

/// Outcome of a successful discovery.
#[derive(Clone, Debug)]
pub struct ToolchainInfo {
    /// The discovered Lean prefix directory (`<prefix>/include/lean/lean.h` exists).
    pub prefix: PathBuf,
    /// `<prefix>/bin/lean` if present on disk.
    pub lean_binary: Option<PathBuf>,
    /// `<prefix>/include/lean/lean.h`.
    pub header_path: PathBuf,
    /// `<prefix>/lib`.
    pub lib_dir: PathBuf,
    /// Version string parsed at discovery time (from a `lean-toolchain` file,
    /// `version.h`, or `lean --version`). Falls back to `LEAN_VERSION` from
    /// `lean-rs-sys` when no live source is available.
    pub version: String,
    /// Build-baked fingerprint (does not vary with discovery results).
    pub fingerprint: ToolchainFingerprint,
    /// Which probe won.
    pub source: DiscoverySource,
}

/// Resolve a Lean toolchain following the documented probe precedence.
///
/// # Errors
///
/// Returns `LinkDiagnostics::MissingLean { tried }` if no probe yields a
/// directory containing `include/lean/lean.h`. Each entry in `tried` is one
/// line describing why a probe was skipped or which path it inspected.
pub fn discover_toolchain(opts: &DiscoverOptions) -> Result<ToolchainInfo, LinkDiagnostics> {
    let mut tried: Vec<String> = Vec::new();

    if let Some(sysroot) = opts.explicit_sysroot.as_ref() {
        if has_header(sysroot) {
            return Ok(build_info(sysroot.clone(), DiscoverySource::ExplicitSysroot, opts));
        }
        tried.push(format!(
            "explicit_sysroot={} (no include/lean/lean.h)",
            sysroot.display()
        ));
    } else {
        tried.push("explicit_sysroot unset".into());
    }

    match env::var_os("LEAN_SYSROOT") {
        Some(value) => {
            let path = PathBuf::from(&value);
            if has_header(&path) {
                return Ok(build_info(path, DiscoverySource::LeanSysrootEnv, opts));
            }
            tried.push(format!("LEAN_SYSROOT={} (no include/lean/lean.h)", path.display()));
        }
        None => tried.push("LEAN_SYSROOT unset".into()),
    }

    if opts.allow_elan {
        match elan_prefix() {
            Ok(Some(path)) => {
                if has_header(&path) {
                    return Ok(build_info(path, DiscoverySource::ElanHome, opts));
                }
                tried.push(format!("elan toolchain {} (no include/lean/lean.h)", path.display()));
            }
            Ok(None) => tried.push("ELAN_HOME unset or elan unavailable".into()),
            Err(reason) => tried.push(reason),
        }
    } else {
        tried.push("elan probe disabled".into());
    }

    if opts.allow_path_lookup {
        match lean_print_prefix() {
            Ok(Some(path)) => {
                if has_header(&path) {
                    return Ok(build_info(path, DiscoverySource::Path, opts));
                }
                tried.push(format!(
                    "`lean --print-prefix` = {} (no include/lean/lean.h)",
                    path.display()
                ));
            }
            Ok(None) => tried.push("`lean --print-prefix` returned nothing".into()),
            Err(reason) => tried.push(reason),
        }
    } else {
        tried.push("PATH lookup disabled".into());
    }

    if opts.allow_lake_env {
        match lake_fixture_prefix() {
            Ok(Some(path)) => {
                if has_header(&path) {
                    return Ok(build_info(path, DiscoverySource::LakeFixtureEnv, opts));
                }
                tried.push(format!(
                    "lake fixture prefix {} (no include/lean/lean.h)",
                    path.display()
                ));
            }
            Ok(None) => tried.push("lake fixture probe: no workspace `fixtures/lean` found".into()),
            Err(reason) => tried.push(reason),
        }
    } else {
        tried.push("lake env probe disabled".into());
    }

    Err(LinkDiagnostics::MissingLean { tried })
}

fn has_header(prefix: &Path) -> bool {
    prefix.join("include").join("lean").join("lean.h").is_file()
}

fn build_info(prefix: PathBuf, source: DiscoverySource, opts: &DiscoverOptions) -> ToolchainInfo {
    let header_path = prefix.join("include").join("lean").join("lean.h");
    let lib_dir = prefix.join("lib");
    let lean_binary = {
        let candidate = prefix.join("bin").join("lean");
        if candidate.is_file() { Some(candidate) } else { None }
    };
    let version = opts
        .toolchain_file
        .as_deref()
        .and_then(parse_toolchain_file)
        .or_else(|| parse_version_header(&prefix))
        .unwrap_or_else(|| lean_rs_sys::LEAN_VERSION.to_string());
    ToolchainInfo {
        prefix,
        lean_binary,
        header_path,
        lib_dir,
        version,
        fingerprint: ToolchainFingerprint::current(),
        source,
    }
}

fn parse_toolchain_file(path: &Path) -> Option<String> {
    let text = fs::read_to_string(path).ok()?;
    // Expected shape: `leanprover/lean4:v4.X.Y` (single line). Any version
    // string is accepted; the caller validates against the supported window.
    let line = text.lines().next()?.trim();
    let (_channel, tag) = line.split_once(':')?;
    Some(tag.trim_start_matches('v').to_string())
}

fn parse_version_header(prefix: &Path) -> Option<String> {
    let version_h = prefix.join("include").join("lean").join("version.h");
    let text = fs::read_to_string(&version_h).ok()?;
    for line in text.lines() {
        if let Some(rest) = line.trim().strip_prefix("#define LEAN_VERSION_STRING") {
            let trimmed = rest.trim().trim_matches('"');
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }
    None
}

fn elan_prefix() -> Result<Option<PathBuf>, String> {
    let Some(elan_home) = env::var_os("ELAN_HOME") else {
        return Ok(None);
    };
    let output = Command::new("elan")
        .args(["show", "active-toolchain"])
        .output()
        .map_err(|err| format!("`elan show active-toolchain` failed: {err}"))?;
    if !output.status.success() {
        return Err(format!("`elan show active-toolchain` exited {}", output.status));
    }
    let toolchain = String::from_utf8_lossy(&output.stdout)
        .lines()
        .next()
        .unwrap_or("")
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_string();
    if toolchain.is_empty() {
        return Ok(None);
    }
    Ok(Some(PathBuf::from(elan_home).join("toolchains").join(toolchain)))
}

fn lean_print_prefix() -> Result<Option<PathBuf>, String> {
    let output = Command::new("lean")
        .arg("--print-prefix")
        .output()
        .map_err(|err| format!("`lean --print-prefix` failed: {err}"))?;
    if !output.status.success() {
        return Err(format!("`lean --print-prefix` exited {}", output.status));
    }
    let prefix = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if prefix.is_empty() {
        Ok(None)
    } else {
        Ok(Some(PathBuf::from(prefix)))
    }
}

fn lake_fixture_prefix() -> Result<Option<PathBuf>, String> {
    let Some(fixture_dir) = find_workspace_fixture() else {
        return Ok(None);
    };
    let output = Command::new("lake")
        .args(["env", "printenv", "LEAN_SYSROOT"])
        .current_dir(&fixture_dir)
        .output()
        .map_err(|err| format!("`lake env printenv LEAN_SYSROOT` failed: {err}"))?;
    if !output.status.success() {
        return Err(format!("`lake env printenv LEAN_SYSROOT` exited {}", output.status));
    }
    let prefix = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if prefix.is_empty() {
        Ok(None)
    } else {
        Ok(Some(PathBuf::from(prefix)))
    }
}

fn find_workspace_fixture() -> Option<PathBuf> {
    // CARGO_MANIFEST_DIR is set during cargo invocations (build + test); at
    // arbitrary runtime we fall back to the current dir.
    let start = env::var_os("CARGO_MANIFEST_DIR")
        .map(PathBuf::from)
        .or_else(|| env::current_dir().ok())?;
    let mut cursor: &Path = start.as_path();
    loop {
        let candidate = cursor.join("fixtures").join("lean");
        if candidate.join("lakefile.lean").is_file() {
            return Some(candidate);
        }
        cursor = cursor.parent()?;
    }
}
