//! Public-surface layout guarantees. The crate-private repr structs are
//! exercised by `#[cfg(test)] mod tests` blocks inside `src/repr.rs`.

// Tests assert structural invariants; panicking on failure is the desired
// behaviour.
#![allow(clippy::panic, clippy::expect_used)]

#[test]
fn opaque_lean_object_is_zero_sized() {
    assert_eq!(core::mem::size_of::<lean_rs_sys::lean_object>(), 0);
}

#[test]
fn version_and_digest_constants_resolve() {
    assert!(!lean_rs_sys::LEAN_VERSION.is_empty());
    assert!(!lean_rs_sys::LEAN_RESOLVED_VERSION.is_empty());
    assert!(!lean_rs_sys::LEAN_HEADER_PATH.is_empty());
    assert_eq!(lean_rs_sys::LEAN_HEADER_DIGEST.len(), 64);
}

#[test]
fn resolved_version_is_in_supported_window() {
    let entry = lean_rs_sys::supported_for(lean_rs_sys::LEAN_RESOLVED_VERSION).unwrap_or_else(|| {
        panic!(
            "LEAN_RESOLVED_VERSION={} not found in SUPPORTED_TOOLCHAINS; \
             build.rs picks the resolved version from the matching entry, so \
             this should be impossible",
            lean_rs_sys::LEAN_RESOLVED_VERSION,
        )
    });
    assert_eq!(entry.header_digest, lean_rs_sys::LEAN_HEADER_DIGEST);
}

#[test]
fn discovered_version_matches_resolved_or_aliases_it() {
    // LEAN_VERSION may differ from LEAN_RESOLVED_VERSION when several
    // releases share one `lean.h` digest; in that case the discovered
    // version still appears in the resolved entry's `versions` array.
    let entry = lean_rs_sys::supported_by_digest(lean_rs_sys::LEAN_HEADER_DIGEST)
        .expect("LEAN_HEADER_DIGEST must match a SUPPORTED_TOOLCHAINS entry");
    assert!(
        entry.includes(lean_rs_sys::LEAN_VERSION) || lean_rs_sys::LEAN_VERSION == "unknown",
        "discovered LEAN_VERSION={} is not listed under the matched entry's versions ({:?})",
        lean_rs_sys::LEAN_VERSION,
        entry.versions,
    );
}
