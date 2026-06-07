//! Compatibility re-exports for the supported Lean toolchain window.
//!
//! The link-free source of truth is `lean-rs-abi`; `lean-rs-sys` keeps this
//! module so existing callers can keep using `lean_rs_sys::supported::*`.

pub use lean_rs_abi::supported::*;
