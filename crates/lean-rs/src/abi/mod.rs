//! Typed first-order ABI conversions for `lean-rs`.
//!
//! [`LeanAbi`](traits::LeanAbi) and [`LeanBoxedAbi`](traits::LeanBoxedAbi)
//! require unsafe implementations for native conversions. Actual C slot types
//! are sealed; native calls still require an explicit value/layout contract.
//!
//! [`traits::IntoLean`], [`traits::TryFromLean`], [`nat`], and [`structure`]
//! provide safe owned-data construction and checked inspection. They do not
//! establish native signatures or permit arbitrary objects to become semantic
//! handles. The concrete safe host boundary lives in [`crate::host_bridge`].

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
mod native;
pub(crate) mod option;
pub(crate) mod scalar;
pub(crate) mod string;
pub mod structure;
pub mod traits;
pub(crate) mod tuple;

#[cfg(test)]
mod tests;
