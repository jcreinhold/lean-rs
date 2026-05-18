//! Typed first-order ABI conversions for `lean-rs`.
//!
//! The whole module is `pub(crate)` per `RD-2026-05-17-004`. The traits
//! [`traits::IntoLean`] and [`traits::TryFromLean`] are the infrastructure
//! that the `module` and `host` layers drive their argument marshalling
//! and return decoding through; they never appear in the public surface.
//!
//! Trait imports for sibling modules:
//!
//! ```ignore
//! use crate::abi::traits::{IntoLean, TryFromLean};
//! ```
//!
//! What this module covers:
//!
//! - `()`, `bool`, `u8`/`u16`/`u32`/`u64`/`usize`, `i8`/`i16`/`i32`/`i64`/`isize`,
//!   `char`, `f64` — see [`scalar`].
//! - `Nat` ↔ `u64`/`usize` (scalar fast path + bignum diagnostic) — see
//!   [`nat`].
//! - `Int` ↔ `i64`/`isize` (scalar fast path + bignum diagnostic) — see
//!   [`int`].
//! - `String` ↔ Rust `String`/`&str` (the [`borrow_str`](string::borrow_str)
//!   helper avoids the Rust-side copy) — see [`string`].
//! - `ByteArray` ↔ Rust `Vec<u8>`/`&[u8]` (the
//!   [`borrow_bytes`](bytearray::borrow_bytes) helper avoids the Rust-side
//!   copy) — see [`bytearray`].
//! - `Array α` ↔ Rust `Vec<T>` (preallocated, single-allocation
//!   construction) — see [`mod@array`].
//! - `Option α` ↔ Rust `Option<T>` — see [`option`].
//! - `Except ε α` ↔ Rust [`Result<T, E>`] **and** the internal value-type
//!   mirror [`Except`](except::Except) — see [`except`].
//! - Product/sum structures via the ctor-layout primitives
//!   [`alloc_ctor_with_objects`](structure::alloc_ctor_with_objects),
//!   [`take_ctor_objects`](structure::take_ctor_objects), and
//!   [`ctor_tag`](structure::ctor_tag) — see [`structure`].

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
pub(crate) mod nat;
pub(crate) mod option;
pub(crate) mod scalar;
pub(crate) mod string;
pub(crate) mod structure;
pub(crate) mod traits;

#[cfg(test)]
mod tests;
