//! Compatibility re-exports for ABI constants.
//!
//! The link-free source of truth is `lean-rs-abi`; `lean-rs-sys` re-exports
//! the constants here so existing callers can keep using
//! `lean_rs_sys::consts::*`.

pub use lean_rs_abi::consts::*;
