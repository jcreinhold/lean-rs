//! Lean 4 toolchain discovery, fingerprinting, allowlist, and build-script helpers.
//!
//! Sits one layer above the external [`lean-sys`](https://crates.io/crates/lean-sys) crate (digama0/Mario Carneiro).
//! Owns the typed Lean version, header SHA256 digest, Lake fixture digest, composed `ToolchainFingerprint`, curated
//! `lean_*` symbol allowlist with a link-time verification test, layered link diagnostics, and reusable build helpers
//! that downstream embedders can call from their own `build.rs` to emit consistent link/rerun directives.
//!
//! `lean-sys` covers ~196 raw C ABI declarations and library linking, but does not surface Lean version metadata,
//! header digests, `cargo:rerun-if-changed` directives, or typed link diagnostics. Generic improvements to that gap
//! are intended to be upstreamed as PRs to digama0/lean-sys; project-specific surfaces (typed fingerprint, allowlist,
//! Lake fixture discovery, layered diagnostics) live here.
//!
//! The public surface is empty at the bootstrap stage. Revised prompts 04R and 05R land the fingerprint, allowlist,
//! discovery, and build-helper surfaces.

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
