//! Safe Rust bindings for hosting Lean 4 capabilities.
//!
//! The single safe front door of the `lean4-rs` project. Internally organized into modules that mirror the original
//! prompt-sequence layering — `runtime` (initialization, owned/borrowed object handles, reference counting, thread
//! guards), `abi` (typed first-order value conversions), `module` (compiled-module loading and exported functions),
//! `host` (high-level capability API: import sessions, calls into Lean-authored capabilities, bounded
//! `MetaM`/`CoreM` services), and `batch` (bulk calls and session pooling).
//!
//! Lean owns elaboration, kernel checking, proof objects, universes, `MetaM`, and dependent-type meaning. This crate
//! owns linking, runtime initialization, ABI conversion, module loading, error/panic boundaries, scheduling,
//! diagnostics, batching, and packaging. Raw Lean 4 C ABI symbols enter the workspace via the external
//! [`lean-sys`](https://crates.io/crates/lean-sys) crate (digama0/Mario Carneiro); this crate does not re-export
//! them.
//!
//! The public surface is empty at the bootstrap stage. Revised prompts 06 onward fill in the runtime, ABI, module,
//! host, and batching modules as discrete sessions.

/// Version of the `lean-rs` crate, matching `Cargo.toml`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    use super::VERSION;

    #[test]
    fn version_constant_matches_package() {
        assert_eq!(VERSION, env!("CARGO_PKG_VERSION"));
    }
}
