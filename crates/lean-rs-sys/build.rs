//! Build script for `lean-rs-sys`.
//!
//! 1. Discover a Lean 4 toolchain prefix.
//! 2. Read `<prefix>/include/lean/lean.h`, compute its SHA-256 digest, and assert
//!    it matches `EXPECTED_HEADER_DIGEST` — the digest the inline mirrors in
//!    `src/refcount.rs`, `src/object.rs`, etc. were authored against.
//! 3. Emit `cargo:rustc-env=…` so `src/consts.rs` can `env!("…")` the resolved
//!    version, header path, and digests.
//! 4. Emit `cargo:rustc-link-search` / `rustc-link-lib` directives for the
//!    selected feature combination (`static` vs `dynamic`, with or without
//!    `mimalloc`).
//! 5. Emit `cargo:rerun-if-*` for the discovery inputs.

// Build scripts panic to abort the build with a diagnostic — that is the
// correct failure mode here, not a smell.
#![allow(clippy::panic, clippy::manual_assert)]

use sha2::{Digest, Sha256};
use std::env;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Digest of `lean.h` from the Lean 4.29.1 toolchain. The inline refcount /
/// layout helpers in this crate were authored against this exact header; if
/// the active toolchain's header digest differs, the build fails with a
/// message naming both digests so the mirrors can be reviewed.
const EXPECTED_HEADER_DIGEST: &str = "2e481a0dac7215eb16123eaef97298ae5a6d0bd0c28c534c2818e2d2f2a28efc";

fn main() {
    if env::var_os("CARGO_FEATURE_STATIC").is_some() && env::var_os("CARGO_FEATURE_DYNAMIC").is_some() {
        panic!(
            "lean-rs-sys: features `static` and `dynamic` are mutually exclusive; \
             pick one (or rely on the default `static`)."
        );
    }

    let prefix = discover_lean_prefix();
    println!(
        "cargo:warning=lean-rs-sys: using Lean toolchain prefix {}",
        prefix.display()
    );

    let header_path = prefix.join("include").join("lean").join("lean.h");
    let actual_digest = sha256_file(&header_path);
    if actual_digest != EXPECTED_HEADER_DIGEST {
        panic!(
            "lean-rs-sys: lean.h at {} has digest {}, but the refcount mirrors were \
             authored against {}. Either pin the supported Lean range or update the \
             mirrors and bump EXPECTED_HEADER_DIGEST.",
            header_path.display(),
            actual_digest,
            EXPECTED_HEADER_DIGEST,
        );
    }

    let version = read_lean_version(&prefix);

    println!("cargo:rustc-env=LEAN_VERSION={version}");
    println!("cargo:rustc-env=LEAN_HEADER_PATH={}", header_path.display());
    println!("cargo:rustc-env=LEAN_HEADER_DIGEST={actual_digest}");
    println!("cargo:rustc-env=LEAN_EXPECTED_HEADER_DIGEST={EXPECTED_HEADER_DIGEST}");

    emit_link_directives(&prefix);

    println!("cargo:rerun-if-env-changed=LEAN_SYSROOT");
    println!("cargo:rerun-if-env-changed=ELAN_HOME");
    println!("cargo:rerun-if-env-changed=PATH");
    println!("cargo:rerun-if-changed={}", header_path.display());
    println!("cargo:rerun-if-changed=build.rs");
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
    println!("cargo:rustc-link-search=native={}", lib_lean.display());
    println!("cargo:rustc-link-search=native={}", lib.display());

    let dynamic = env::var_os("CARGO_FEATURE_DYNAMIC").is_some();
    let static_link = env::var_os("CARGO_FEATURE_STATIC").is_some() && !dynamic;

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
