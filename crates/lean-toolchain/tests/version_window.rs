//! End-to-end check that the toolchain this binary was built against
//! is inside [`lean_rs_sys::SUPPORTED_TOOLCHAINS`].
//!
//! `lean-rs-sys`'s `build.rs` already fails the build when the active
//! toolchain's `lean.h` digest is outside the window, so this test is
//! a runtime backstop: it proves the resolved version is reachable via
//! [`lean_rs_sys::supported_for`] and the matched entry's digest agrees
//! with [`lean_rs_sys::LEAN_HEADER_DIGEST`].

#![allow(clippy::panic, clippy::expect_used)]

#[test]
fn baked_toolchain_is_in_the_supported_window() {
    let fp = lean_toolchain::ToolchainFingerprint::current();
    assert!(
        fp.is_supported(),
        "build-time toolchain {} is not in lean_rs_sys::SUPPORTED_TOOLCHAINS \
         (resolved: {}, digest: {})",
        fp.lean_version,
        fp.resolved_version,
        fp.header_sha256,
    );

    let entry = lean_rs_sys::supported_for(fp.lean_version).unwrap_or_else(|| {
        panic!(
            "supported_for({}) returned None despite is_supported()==true",
            fp.lean_version,
        )
    });
    assert_eq!(
        entry.header_digest, fp.header_sha256,
        "matched entry's digest ({}) disagrees with build-time header digest ({})",
        entry.header_digest, fp.header_sha256,
    );
}

#[test]
fn resolved_version_aliases_into_the_matched_entry() {
    let fp = lean_toolchain::ToolchainFingerprint::current();
    let entry = lean_rs_sys::supported_for(fp.lean_version).expect("baked version must be supported");
    assert!(
        entry.versions.contains(&fp.resolved_version),
        "resolved version {} is not listed in the matched entry's versions {:?}",
        fp.resolved_version,
        entry.versions,
    );
}

#[test]
fn required_symbols_are_present_across_the_window() {
    // Every entry in REQUIRED_SYMBOLS must be marked present in every
    // window entry. If we ever add a `missing_symbols` slot, the
    // window-wide guarantee is the unit this asserts.
    for &symbol in lean_rs_sys::REQUIRED_SYMBOLS {
        assert!(
            lean_rs_sys::symbol_in_all(symbol),
            "{symbol} is not marked present in every supported toolchain",
        );
    }
}

#[test]
fn unknown_versions_are_not_supported() {
    assert!(lean_rs_sys::supported_for("0.0.0").is_none());
    assert!(lean_rs_sys::supported_for("4.999.999").is_none());
    assert!(lean_rs_sys::supported_by_digest("deadbeef").is_none());
}
