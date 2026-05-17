//! Smoke checks for the build-baked metadata constants.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use lean_toolchain::{
    HOST_TRIPLE, LAKE_FIXTURE_DIGEST, LEAN_HEADER_DIGEST, LEAN_HEADER_PATH, LEAN_VERSION, ToolchainFingerprint, VERSION,
};

fn is_lower_hex_64(s: &str) -> bool {
    s.len() == 64 && s.chars().all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
}

#[test]
fn crate_version_is_set() {
    assert!(!VERSION.is_empty());
}

#[test]
fn lean_version_looks_like_a_lean4_version() {
    assert!(LEAN_VERSION.starts_with('4'), "got {LEAN_VERSION}");
    assert!(LEAN_VERSION.contains('.'), "got {LEAN_VERSION}");
}

#[test]
fn lean_header_digest_is_lowercase_sha256() {
    assert!(is_lower_hex_64(LEAN_HEADER_DIGEST), "got {LEAN_HEADER_DIGEST}");
}

#[test]
fn lean_header_path_points_at_lean_h() {
    assert!(
        LEAN_HEADER_PATH.ends_with("include/lean/lean.h"),
        "got {LEAN_HEADER_PATH}"
    );
}

#[test]
fn fixture_digest_is_lowercase_sha256() {
    assert!(is_lower_hex_64(LAKE_FIXTURE_DIGEST), "got {LAKE_FIXTURE_DIGEST}");
}

#[test]
fn host_triple_is_nonempty() {
    assert!(!HOST_TRIPLE.is_empty());
}

#[test]
fn fingerprint_composes_from_the_baked_consts() {
    let fp = ToolchainFingerprint::current();
    assert_eq!(fp.lean_version, LEAN_VERSION);
    assert_eq!(fp.header_sha256, LEAN_HEADER_DIGEST);
    assert_eq!(fp.fixture_sha256, LAKE_FIXTURE_DIGEST);
    assert_eq!(fp.host_triple, HOST_TRIPLE);
}
