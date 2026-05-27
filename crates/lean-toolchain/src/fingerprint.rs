//! Typed composition of the build-baked Lean toolchain identity.
//!
//! Every field is a `&'static str` resolved at build time—by
//! `lean-rs-sys`'s build script (`LEAN_VERSION`, `LEAN_HEADER_DIGEST`) or by
//! this crate's `build.rs` (`LAKE_FIXTURE_DIGEST`, `HOST_TRIPLE`). Published
//! crate builds that do not contain the workspace fixture record a zero
//! `LAKE_FIXTURE_DIGEST`; the value is a workspace regression key, not a
//! downstream compatibility promise. The
//! fingerprint is therefore stable across runs for a given build and cheap to
//! use as a cache key.

// `LAKE_FIXTURE_DIGEST` and `HOST_TRIPLE` are emitted as `pub const` by
// `build.rs`.
include!(concat!(env!("OUT_DIR"), "/metadata.rs"));

/// Typed identity of the Lean toolchain this crate was compiled against.
///
/// `Eq + Hash` so the fingerprint can serve as a `HashMap` key for caches
/// keyed by toolchain identity (e.g. compiled module caches, proof caches).
/// `Clone + Debug` are derived for convenience; every field is `&'static str`
/// so cloning is a pointer copy.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct ToolchainFingerprint {
    /// `LEAN_VERSION_STRING` from the active `lean.h`.
    pub lean_version: &'static str,
    /// The version string from the matched
    /// [`SupportedToolchain`](lean_rs_sys::SupportedToolchain) entry. Equal
    /// to [`Self::lean_version`] except when several releases share one
    /// `lean.h` digest, in which case it is the first version listed for
    /// that entry.
    pub resolved_version: &'static str,
    /// SHA-256 of the `lean.h` this build was resolved against.
    pub header_sha256: &'static str,
    /// SHA-256 of the workspace Lake fixture artifacts, or zero when the
    /// crate is built from a published tarball without workspace fixtures.
    pub fixture_sha256: &'static str,
    /// Target triple this crate was built for.
    pub host_triple: &'static str,
}

impl ToolchainFingerprint {
    /// Compose the fingerprint from the build-baked constants.
    #[must_use]
    pub const fn current() -> Self {
        Self {
            lean_version: lean_rs_sys::LEAN_VERSION,
            resolved_version: lean_rs_sys::LEAN_RESOLVED_VERSION,
            header_sha256: lean_rs_sys::LEAN_HEADER_DIGEST,
            fixture_sha256: LAKE_FIXTURE_DIGEST,
            host_triple: HOST_TRIPLE,
        }
    }

    /// Return `true` iff [`Self::lean_version`] is included in the
    /// [`SUPPORTED_TOOLCHAINS`](lean_rs_sys::SUPPORTED_TOOLCHAINS) window.
    ///
    /// The build script already filters at compile time, so this method
    /// returns `true` for any binary that compiled successfully. It is
    /// exposed for tooling that constructs a fingerprint from an external
    /// source (e.g. a remote-worker handshake).
    #[must_use]
    pub fn is_supported(&self) -> bool {
        lean_rs_sys::supported_for(self.lean_version).is_some()
    }
}

impl Default for ToolchainFingerprint {
    fn default() -> Self {
        Self::current()
    }
}

#[cfg(test)]
mod tests {
    use super::ToolchainFingerprint;
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash as _, Hasher as _};

    #[test]
    fn current_round_trips_through_clone() {
        let a = ToolchainFingerprint::current();
        let b = a.clone();
        assert_eq!(a, b);
    }

    #[test]
    fn fingerprint_hash_is_deterministic() {
        let a = ToolchainFingerprint::current();
        let b = ToolchainFingerprint::current();
        let mut ha = DefaultHasher::new();
        let mut hb = DefaultHasher::new();
        a.hash(&mut ha);
        b.hash(&mut hb);
        assert_eq!(ha.finish(), hb.finish());
    }

    #[test]
    fn distinct_header_digest_changes_fingerprint() {
        let a = ToolchainFingerprint::current();
        let b = ToolchainFingerprint {
            header_sha256: "0000000000000000000000000000000000000000000000000000000000000000",
            ..a
        };
        assert_ne!(a, b);
    }

    #[test]
    fn current_is_in_supported_window() {
        assert!(ToolchainFingerprint::current().is_supported());
    }

    #[test]
    fn synthetic_unknown_version_is_not_supported() {
        let mut fp = ToolchainFingerprint::current();
        fp.lean_version = "0.0.0-test";
        assert!(!fp.is_supported());
    }
}
