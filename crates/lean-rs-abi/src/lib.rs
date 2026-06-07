//! Link-free Lean 4 ABI and toolchain metadata.
//!
//! This crate owns metadata that both build tooling and raw FFI bindings need:
//! the supported Lean toolchain window, the required Lean runtime symbol names,
//! and the build-time Lean version/header fingerprint. It deliberately does not
//! declare `extern "C"` items and does not emit Lean link directives.

#![forbid(unsafe_code)]

pub mod consts;
pub mod supported;

mod symbols;

pub use consts::{
    LEAN_ARRAY, LEAN_CLOSURE, LEAN_CLOSURE_MAX_ARGS, LEAN_EXTERNAL, LEAN_HEADER_DIGEST, LEAN_HEADER_PATH,
    LEAN_MAX_CTOR_FIELDS, LEAN_MAX_CTOR_SCALARS_SIZE, LEAN_MAX_CTOR_TAG, LEAN_MAX_SMALL_OBJECT_SIZE, LEAN_MPZ,
    LEAN_OBJECT_SIZE_DELTA, LEAN_PROMISE, LEAN_REF, LEAN_RESERVED, LEAN_RESOLVED_VERSION, LEAN_SCALAR_ARRAY,
    LEAN_STRING, LEAN_STRUCT_ARRAY, LEAN_TASK, LEAN_THUNK, LEAN_VERSION,
};
pub use supported::{
    SUPPORTED_TOOLCHAINS, SupportedToolchain, supported_by_digest, supported_for, symbol_present_in_window,
};
pub use symbols::REQUIRED_SYMBOLS;

/// Version of the `lean-rs-abi` crate, matching `Cargo.toml`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Return `true` iff `symbol` is required and present across the window.
///
/// More precisely: `symbol` is one of [`REQUIRED_SYMBOLS`] and no entry in
/// [`SUPPORTED_TOOLCHAINS`] lists it under `missing_symbols`.
#[must_use]
pub fn symbol_in_all(symbol: &str) -> bool {
    REQUIRED_SYMBOLS.contains(&symbol) && symbol_present_in_window(symbol)
}

#[cfg(test)]
mod tests {
    use super::{LEAN_HEADER_DIGEST, LEAN_VERSION, REQUIRED_SYMBOLS, VERSION, symbol_in_all};

    #[test]
    fn version_constant_matches_package() {
        assert_eq!(VERSION, env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn required_symbols_are_nonempty() {
        assert!(!REQUIRED_SYMBOLS.is_empty());
    }

    #[test]
    fn required_symbols_are_present_across_the_window() {
        for &symbol in REQUIRED_SYMBOLS {
            assert!(
                symbol_in_all(symbol),
                "{symbol} is not marked present in every supported toolchain",
            );
        }
    }

    #[test]
    fn lean_metadata_is_baked() {
        assert!(!LEAN_VERSION.is_empty());
        assert_eq!(LEAN_HEADER_DIGEST.len(), 64);
        assert!(LEAN_HEADER_DIGEST.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
