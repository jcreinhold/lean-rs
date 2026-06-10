//! Link-free Lean 4 ABI and toolchain metadata.
//!
//! This crate owns the static metadata that both build tooling and raw FFI
//! bindings need: the supported Lean toolchain window, the required Lean
//! runtime symbol names, and the `lean.h` layout constants. It is purely
//! static — no build script, no `extern "C"` items, no link directives, and no
//! probe of an installed toolchain — so any crate can depend on it without a
//! Lean toolchain present. Live toolchain identity (installed version, header
//! path, header digest) belongs to `lean-toolchain`, the crate whose job is
//! toolchain discovery.

#![forbid(unsafe_code)]

pub mod consts;
pub mod supported;

mod symbols;

pub use consts::{
    LEAN_ARRAY, LEAN_CLOSURE, LEAN_CLOSURE_MAX_ARGS, LEAN_EXTERNAL, LEAN_MAX_CTOR_FIELDS, LEAN_MAX_CTOR_SCALARS_SIZE,
    LEAN_MAX_CTOR_TAG, LEAN_MAX_SMALL_OBJECT_SIZE, LEAN_MPZ, LEAN_OBJECT_SIZE_DELTA, LEAN_PROMISE, LEAN_REF,
    LEAN_RESERVED, LEAN_SCALAR_ARRAY, LEAN_STRING, LEAN_STRUCT_ARRAY, LEAN_TASK, LEAN_THUNK,
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
    use super::{REQUIRED_SYMBOLS, VERSION, symbol_in_all};

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
}
