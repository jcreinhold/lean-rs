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

pub(crate) mod array;
// Only the unit tests and the `fuzzing` entry points build or read a `ByteArray`.
#[cfg(any(test, feature = "fuzzing"))]
pub(crate) mod bytearray;
pub(crate) mod except;
// Only the unit tests convert Lean `Int` values.
#[cfg(test)]
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
