//! Typed first-order ABI conversions for `lean-rs`.
//!
//! The public surface is the sealed [`LeanAbi`](traits::LeanAbi) trait
//! (re-exported at the crate root) plus the unboxing/boxing impls for
//! every Lean type that crosses [`crate::module::LeanExported`]'s typed
//! dispatch. Sealing prevents downstream crates from implementing
//! `LeanAbi` for foreign types—wrong impls would produce undefined
//! behaviour at the FFI boundary.
//!
//! The submodules ([`nat`], [`structure`], [`traits`]) expose the
//! helpers the sibling `lean-rs-host` crate needs to decode and
//! construct host-defined Lean structures; their items are reachable
//! via `lean_rs::__host_internals` and are not part of `lean-rs`'s
//! public semver promise.

// Items here are infrastructure for the typed `LeanExported` family
// and the `module` / `host` modules. A subset is exercised only by
// `cargo test` (the in-tree `abi::tests` and the integration suites);
// the lib-only `cargo build` cannot prove every helper is reachable.
#![allow(
    dead_code,
    reason = "ABI helpers reached through generic dispatch; lib-only build cannot prove reachability"
)]

pub(crate) mod array;
pub(crate) mod bytearray;
pub(crate) mod except;
pub(crate) mod int;
pub mod nat;
pub(crate) mod option;
pub(crate) mod scalar;
pub(crate) mod string;
pub mod structure;
pub mod traits;
pub(crate) mod tuple;

#[cfg(test)]
mod tests;
