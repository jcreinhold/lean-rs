//! Build script for `lean-rs-sys`.
//!
//! 1. Discover a Lean 4 toolchain prefix.
//! 2. Read `<prefix>/include/lean/lean.h`, compute its SHA-256 digest, and
//!    look up the matching [`SupportedToolchain`](crate::SupportedToolchain)
//!    entry. The build fails if no entry matches.
//! 3. Emit `cargo:rustc-env=…` so `src/consts.rs` can `env!("…")` the resolved
//!    version, header path, and digest, plus the version `cfg` flags
//!    (`lean_v_X_Y_Z` exact-equality and `lean_at_least_X_Y` lower-bound) so
//!    downstream code can `#[cfg]`-gate per-version divergences, each paired
//!    with a `rustc-check-cfg` over the whole window so the gates stay
//!    lint-clean for the non-active versions too.
//! 4. Emit `cargo:rustc-link-search` / `rustc-link-lib` directives for the
//!    selected feature combination (`static` vs `dynamic`, with or without
//!    `mimalloc`).
//! 5. Emit `cargo:rerun-if-*` for the discovery inputs.

// Build scripts panic to abort the build with a diagnostic—that is the
// correct failure mode here, not a smell.
#![allow(clippy::panic, clippy::manual_assert)]

use sha2::{Digest, Sha256};
use std::env;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

// `src/supported.rs` is `include!`-ed below so the build script can read
// `SUPPORTED_TOOLCHAINS` without depending on the crate itself. The included
// file references `crate::REQUIRED_SYMBOLS` only inside `#[cfg(test)]`
// helpers, so a build-script include works.
#[path = "src/supported.rs"]
#[allow(dead_code, unreachable_pub)]
mod supported;

use supported::{SUPPORTED_TOOLCHAINS, SupportedToolchain};

fn main() {
    if env::var_os("CARGO_FEATURE_STATIC").is_some() && env::var_os("CARGO_FEATURE_DYNAMIC").is_some() {
        panic!(
            "lean-rs-sys: features `static` and `dynamic` are mutually exclusive; \
             pick one (or rely on the default `static`)."
        );
    }

    if env::var_os("DOCS_RS").is_some() {
        emit_docs_rs_metadata();
        return;
    }

    let prefix = discover_lean_prefix();
    println!(
        "cargo:warning=lean-rs-sys: using Lean toolchain prefix {}",
        prefix.display()
    );

    let header_path = prefix.join("include").join("lean").join("lean.h");
    let actual_digest = sha256_file(&header_path);
    let entry = SUPPORTED_TOOLCHAINS
        .iter()
        .find(|t| t.header_digest == actual_digest)
        .unwrap_or_else(|| {
            panic!(
                "lean-rs-sys: lean.h at {} has digest {} but no entry in \
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

    emit_link_directives(&prefix);

    println!("cargo:rerun-if-env-changed=LEAN_SYSROOT");
    println!("cargo:rerun-if-env-changed=ELAN_HOME");
    println!("cargo:rerun-if-env-changed=PATH");
    println!("cargo:rerun-if-changed={}", header_path.display());
    println!("cargo:rerun-if-changed=src/supported.rs");
    println!("cargo:rerun-if-changed=build.rs");
}

/// docs.rs does not install Lean. Rustdoc still needs the compile-time
/// constants from `src/consts.rs`, but it must not link or probe the local
/// Lean runtime while rendering API docs.
fn emit_docs_rs_metadata() {
    let entry = SUPPORTED_TOOLCHAINS
        .last()
        .unwrap_or_else(|| panic!("lean-rs-sys: SUPPORTED_TOOLCHAINS is empty"));
    let resolved_version = entry
        .versions
        .first()
        .copied()
        .unwrap_or_else(|| panic!("lean-rs-sys: latest supported toolchain entry has no versions"));

    println!("cargo:warning=lean-rs-sys: DOCS_RS=1; emitting documentation metadata without probing Lean");
    println!("cargo:rustc-env=LEAN_VERSION={resolved_version}");
    println!("cargo:rustc-env=LEAN_RESOLVED_VERSION={resolved_version}");
    println!("cargo:rustc-env=LEAN_HEADER_PATH=<docs.rs synthetic lean.h>");
    println!("cargo:rustc-env=LEAN_HEADER_DIGEST={}", entry.header_digest);
    emit_version_cfgs(resolved_version);
    println!("cargo:rerun-if-env-changed=DOCS_RS");
    println!("cargo:rerun-if-changed=src/supported.rs");
    println!("cargo:rerun-if-changed=build.rs");
}

/// Choose which version string from `entry.versions` to record as the
/// "resolved" version. Prefers an exact match with the discovered version;
/// falls back to the entry's first listed version (a still-supported alias
/// for the same `lean.h` digest).
fn pick_resolved_version<'a>(entry: &'a SupportedToolchain, discovered: &'a str) -> &'a str {
    if entry.versions.contains(&discovered) {
        return discovered;
    }
    // The `unwrap_or` fallback guards against an empty `versions` array,
    // which is structurally rejected by `supported.rs`'s
    // `every_entry_lists_at_least_one_version` test but cannot be asserted
    // at build-script compile time.
    entry.versions.first().copied().unwrap_or("unknown")
}

/// Convert a version string to a valid `cfg` token. Examples: `"4.29.1"` →
/// `"4_29_1"` (downstream uses `#[cfg(lean_v_4_29_1)]`); `"4.30.0"` →
/// `"4_30_0"`. Any byte outside `[A-Za-z0-9_]` collapses to `_` so
/// release-candidate suffixes do not produce invalid `--cfg` arguments.
fn cfg_token(version: &str) -> String {
    version
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' { c } else { '_' })
        .collect()
}

/// Extract the `(major, minor)` key from a Lean version string, ignoring the
/// patch level and any `-rcN` / build suffix. `"4.31.0-rc1"` → `(4, 31)`;
/// `"4.30.0"` → `(4, 30)`. Returns `None` when the leading two components are
/// not numeric (e.g. `"unknown"`, a `nightly-*` pin), in which case no
/// lower-bound flag is set.
fn minor_key(version: &str) -> Option<(u32, u32)> {
    let numeric = version.split(['-', '+']).next().unwrap_or(version);
    let mut parts = numeric.split('.');
    let major: u32 = parts.next()?.parse().ok()?;
    let minor: u32 = parts.next()?.parse().ok()?;
    Some((major, minor))
}

/// Emit the version `cfg` flags downstream code gates on, plus a
/// `rustc-check-cfg` for every flag the supported window can produce so
/// `#[cfg(lean_v_*)]` / `#[cfg(lean_at_least_*)]` stay lint-clean even for the
/// versions that are not the active one (cargo only auto-registers the flags
/// it actually sets, not the inactive ones a gate may name).
///
/// Two families:
///
/// - `lean_v_<token>` — exact equality with the resolved version; exactly one
///   is active per build.
/// - `lean_at_least_<major>_<minor>` — a monotone lower-bound predicate, set
///   for every window minor at or below the resolved version. Gate "needs a
///   symbol introduced in 4.31" with `#[cfg(lean_at_least_4_31)]`; gate the
///   pre-4.31 path with `#[cfg(not(lean_at_least_4_31))]`. Release candidates
///   count as their target minor (`4.31.0-rc1` ⇒ `lean_at_least_4_31`).
///
/// Only minors **within** the window `[floor ..= head]` are registered, so a
/// gate may name only boundaries that exist; `#[cfg(not(lean_at_least_4_32))]`
/// for a version above the head is a hard `unexpected_cfgs` error until 4.32
/// joins the window (fail-fast, by design — there is nothing above the head to
/// gate against yet).
fn emit_version_cfgs(resolved_version: &str) {
    use std::collections::BTreeSet;

    // The full set of flags the window can name, gathered once so the
    // `check-cfg` allowlist covers non-active versions a gate might mention.
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

    // The active exact-version flag.
    println!("cargo:rustc-cfg=lean_v_{}", cfg_token(resolved_version));

    // The active lower-bound flags: every window minor at or below the
    // resolved version. A non-numeric resolved version sets none of them.
    if let Some(resolved_key) = minor_key(resolved_version) {
        for &(major, minor) in &minors {
            if (major, minor) <= resolved_key {
                println!("cargo:rustc-cfg=lean_at_least_{major}_{minor}");
            }
        }
    }
}

/// Render the supported window as a multi-line bulleted summary, suitable
/// for inclusion in a panic message.
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
        tried.push("fixture prefix: `lake env --print-prefix` unavailable".into());
    }

    panic!(
        "lean-rs-sys: could not locate a Lean toolchain prefix. Tried:\n  - {}",
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
    let bytes = fs::read(path).unwrap_or_else(|err| panic!("lean-rs-sys: cannot read {}: {err}", path.display()));
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

fn emit_link_directives(prefix: &Path) {
    let lib_lean = prefix.join("lib").join("lean");
    let lib = prefix.join("lib");

    let metadata_only = env::var_os("CARGO_FEATURE_METADATA_ONLY").is_some();
    let dynamic = env::var_os("CARGO_FEATURE_DYNAMIC").is_some();
    let static_link = env::var_os("CARGO_FEATURE_STATIC").is_some() && !dynamic;

    if metadata_only && !dynamic && !static_link {
        return;
    }

    println!("cargo:rustc-link-search=native={}", lib_lean.display());
    println!("cargo:rustc-link-search=native={}", lib.display());

    if static_link {
        for lib in ["Lean", "Init", "leanrt", "leancpp", "Lake"] {
            println!("cargo:rustc-link-lib=static={lib}");
        }
        let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
        match target_os.as_str() {
            "macos" => {
                println!("cargo:rustc-link-lib=dylib=c++");
                println!("cargo:rustc-link-lib=dylib=c++abi");
            }
            "linux" => {
                println!("cargo:rustc-link-lib=dylib=stdc++");
                println!("cargo:rustc-link-lib=dylib=dl");
                println!("cargo:rustc-link-lib=dylib=pthread");
                println!("cargo:rustc-link-lib=dylib=m");
            }
            other => {
                println!("cargo:warning=lean-rs-sys: no platform-specific link directives for target_os={other}");
            }
        }
        println!("cargo:rustc-link-lib=dylib=gmp");
        println!("cargo:rustc-link-lib=dylib=uv");
    } else {
        println!("cargo:rustc-link-lib=dylib=leanshared");
        // Bake an rpath into this crate's own binaries (tests, examples,
        // benches) so they can load `libleanshared.{dylib,so}` at run-time
        // without `DYLD_FALLBACK_LIBRARY_PATH` / `LD_LIBRARY_PATH` set.
        // `cargo:rustc-link-arg` only affects the package emitting it, so
        // dependent crates that produce their own binaries need to emit
        // the same flag from their own build script (see
        // `crates/lean-rs/build.rs`).
        let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
        if matches!(target_os.as_str(), "macos" | "linux") {
            println!("cargo:rustc-link-arg=-Wl,-rpath,{}", lib_lean.display());
        }
    }

    // Lean's mimalloc is statically linked into `libleanrt.a` /
    // `libleanshared`; the `mimalloc` feature is preserved as a no-op marker
    // for downstream embedders who may want to gate behaviour on it. When
    // Lean ships a separate `libmimalloc` (e.g. via a custom build), this is
    // the place to add the extra `cargo:rustc-link-lib=` directive.
    if env::var_os("CARGO_FEATURE_MIMALLOC").is_some() {
        // intentional no-op; presence noted for telemetry only.
    }
}
