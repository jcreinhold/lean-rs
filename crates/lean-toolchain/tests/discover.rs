//! Integration tests for `discover_toolchain` and `LinkDiagnostics`.
//!
//! Tests deliberately avoid mutating process environment variables: in modern
//! Rust `std::env::set_var` is `unsafe` and the workspace denies `unsafe`
//! outside `lean-rs-abi`. Where a probe's behaviour depends on ambient env
//! state, the test is gated on the observed state at startup.

#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::unwrap_used,
    clippy::wildcard_enum_match_arm
)]

use std::env;
use std::path::{Path, PathBuf};

use lean_toolchain::{DiscoverOptions, DiscoverySource, LinkDiagnostics, ToolchainFingerprint, discover_toolchain};

/// `<LEAN_HEADER_PATH>` is `<prefix>/include/lean/lean.h`. Three `parent()`
/// hops recover `<prefix>`—the build-resolved Lean toolchain root.
fn baked_prefix() -> PathBuf {
    let header = Path::new(lean_toolchain::LEAN_HEADER_PATH);
    header
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .expect("LEAN_HEADER_PATH should be <prefix>/include/lean/lean.h")
}

#[test]
fn explicit_sysroot_wins_over_every_other_probe() {
    let prefix = baked_prefix();
    let info = discover_toolchain(&DiscoverOptions {
        explicit_sysroot: Some(prefix.clone()),
        allow_lean_sysroot_env: false,
        allow_path_lookup: false,
        allow_elan: false,
        allow_lake_env: false,
        toolchain_file: None,
    })
    .expect("baked prefix should contain a Lean header");

    assert_eq!(info.source, DiscoverySource::ExplicitSysroot);
    assert_eq!(info.prefix, prefix);
    assert!(info.header_path.is_file());
    assert_eq!(info.lib_dir, prefix.join("lib"));
    assert_eq!(info.fingerprint, ToolchainFingerprint::current());
}

#[test]
fn invalid_explicit_sysroot_with_all_probes_off_reports_missing_lean() {
    // With every ambient probe—including the `LEAN_SYSROOT` env probe—disabled,
    // a bogus explicit sysroot must fail deterministically regardless of the
    // ambient environment (CI sets `LEAN_SYSROOT`).
    let err = discover_toolchain(&DiscoverOptions {
        explicit_sysroot: Some(PathBuf::from("/definitely/does/not/exist/lean-prefix")),
        allow_lean_sysroot_env: false,
        allow_path_lookup: false,
        allow_elan: false,
        allow_lake_env: false,
        toolchain_file: None,
    })
    .expect_err("with every probe off and a bogus explicit sysroot, discovery must fail");

    match err {
        LinkDiagnostics::MissingLean { tried } => {
            assert!(!tried.is_empty(), "tried list should record each probe attempt");
            for line in &tried {
                assert!(
                    !line.contains('\n'),
                    "each `tried` entry should be a single line, got: {line:?}"
                );
            }
        }
        other => panic!("expected MissingLean, got {other:?}"),
    }
}

#[test]
fn path_lookup_succeeds_when_lean_is_on_path() {
    // Gate: a Lean toolchain *must* be reachable through the env, otherwise
    // there's nothing to assert. `lean-rs-abi`'s build script also relies on
    // this—running the test suite already requires a working toolchain.
    let opts = DiscoverOptions {
        explicit_sysroot: None,
        allow_lean_sysroot_env: true,
        allow_path_lookup: true,
        allow_elan: false,
        allow_lake_env: false,
        toolchain_file: None,
    };
    match discover_toolchain(&opts) {
        Ok(info) => {
            // Source may be LeanSysrootEnv if the env var is set; either way
            // the discovered prefix must contain the header.
            assert!(info.header_path.is_file());
            assert!(matches!(
                info.source,
                DiscoverySource::Path | DiscoverySource::LeanSysrootEnv
            ));
        }
        Err(err) => panic!("PATH discovery should succeed in a Lean-equipped test environment: {err}"),
    }
}

#[test]
fn missing_lean_diagnostic_displays_on_one_line() {
    let diag = LinkDiagnostics::MissingLean {
        tried: vec![
            "explicit_sysroot unset".to_string(),
            "LEAN_SYSROOT unset".to_string(),
            "PATH lookup disabled".to_string(),
        ],
    };
    let rendered = format!("{diag}");
    assert!(!rendered.contains('\n'), "single-line message expected: {rendered}");
    assert!(rendered.contains("explicit_sysroot unset"));
    assert!(rendered.contains("PATH lookup disabled"));
}

#[test]
fn fixture_artifact_missing_diagnostic_includes_recovery() {
    let diag = LinkDiagnostics::FixtureArtifactMissing {
        path: PathBuf::from("/tmp/missing.olean"),
        recovery: "cd fixtures/lean && lake build",
    };
    let rendered = format!("{diag}");
    assert!(rendered.contains("/tmp/missing.olean"));
    assert!(rendered.contains("cd fixtures/lean && lake build"));
    assert!(!rendered.contains('\n'));
}

#[test]
fn allowlist_failure_diagnostic_names_the_symbol() {
    let diag = LinkDiagnostics::AllowlistFailure { name: "lean_apply_42" };
    assert!(format!("{diag}").contains("lean_apply_42"));
}

#[test]
fn toolchain_file_overrides_version_string() {
    use std::fs;
    let tmp = env::temp_dir().join(format!("lean-toolchain-test-{}.txt", std::process::id()));
    fs::write(&tmp, "leanprover/lean4:v4.99.0\n").expect("write tmp toolchain file");

    let info = discover_toolchain(&DiscoverOptions {
        explicit_sysroot: Some(baked_prefix()),
        allow_lean_sysroot_env: false,
        allow_path_lookup: false,
        allow_elan: false,
        allow_lake_env: false,
        toolchain_file: Some(tmp.clone()),
    })
    .expect("explicit sysroot should resolve");

    drop(fs::remove_file(&tmp));
    assert_eq!(info.version, "4.99.0");
}
