//! Public-surface layout guarantees. The crate-private repr structs are
//! exercised by `#[cfg(test)] mod tests` blocks inside `src/repr.rs`.

#[test]
fn opaque_lean_object_is_zero_sized() {
    assert_eq!(core::mem::size_of::<lean_rs_sys::lean_object>(), 0);
}

#[test]
fn version_and_digest_constants_resolve() {
    assert!(!lean_rs_sys::LEAN_VERSION.is_empty());
    assert!(!lean_rs_sys::LEAN_HEADER_PATH.is_empty());
    assert_eq!(lean_rs_sys::LEAN_HEADER_DIGEST.len(), 64);
    assert_eq!(lean_rs_sys::EXPECTED_HEADER_DIGEST.len(), 64);
    // The build only succeeds when the digests match; we re-assert here so
    // anyone reading the test output sees the equality.
    assert_eq!(
        lean_rs_sys::LEAN_HEADER_DIGEST,
        lean_rs_sys::EXPECTED_HEADER_DIGEST,
    );
}
