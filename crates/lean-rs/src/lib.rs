//! Safe Rust bindings for hosting Lean 4 capabilities.
//!
//! The single safe front door of the `lean-rs` project. Internally organized into modules that mirror the original
//! prompt-sequence layering — `runtime` (initialization, owned/borrowed object handles, reference counting, thread
//! guards), `abi` (typed first-order value conversions), `module` (compiled-module loading and exported functions),
//! `host` (high-level capability API: import sessions, calls into Lean-authored capabilities, bounded
//! `MetaM`/`CoreM` services), and `batch` (bulk calls and session pooling).
//!
//! Lean owns elaboration, kernel checking, proof objects, universes, `MetaM`, and dependent-type meaning. This crate
//! owns linking, runtime initialization, ABI conversion, module loading, error/panic boundaries, scheduling,
//! diagnostics, batching, and packaging. Raw Lean 4 C ABI symbols enter the workspace via the in-tree
//! `lean-rs-sys` crate (`publish = false`); this crate consumes them in `pub(crate)` modules and does not
//! re-export them.
//!
//! The public surface is empty at the bootstrap stage. Prompts 06 onward fill in the runtime, ABI, module,
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
