//! Lean 4 toolchain discovery, fingerprinting, allowlist re-export, and build-script helpers.
//!
//! Sits one layer above the in-tree workspace crate `lean-rs-sys` (`publish = false`). `lean-rs-sys` owns the raw
//! `extern "C"` declarations, the hand-written refcount inline helpers, the signature-checked symbol allowlist,
//! the header SHA-256 digest, and the link directives. This crate composes on top: the typed
//! `ToolchainFingerprint`, the Lake fixture digest, the layered `LinkDiagnostics`, and the reusable build helpers
//! that downstream embedders can call from their own `build.rs` to emit consistent link/rerun directives.
//!
//! The public surface is empty at the bootstrap stage. Prompts 04 and 05 land the raw bindings, the fingerprint,
//! the discovery surface, and the build-helper APIs.

/// Version of the `lean-toolchain` crate, matching `Cargo.toml`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    use super::VERSION;

    #[test]
    fn version_constant_matches_package() {
        assert_eq!(VERSION, env!("CARGO_PKG_VERSION"));
    }
}
