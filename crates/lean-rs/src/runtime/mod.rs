//! Process-wide Lean runtime anchor (`pub(crate)` infrastructure).
//!
//! The `runtime` module is `pub(crate)` per
//! `docs/architecture/03-host-api.md` and `RD-2026-05-17-004`. Only
//! [`LeanRuntime`] is re-exported at the crate root; every other item in
//! this module is internal scaffolding (init cell, panic boundary, thread
//! attach RAII, and — landing in prompt 07 — the lifetime-bound object
//! handles `Obj<'lean>` / `ObjRef<'lean, 'a>`).
//!
//! What this layer hides from the rest of the crate:
//!
//! - which raw `lean_rs_sys` symbols implement init and thread attach;
//! - the order in which those symbols must be called;
//! - the `OnceLock` cell that makes initialization process-once;
//! - the `catch_unwind` boundary that keeps Rust panics from unwinding
//!   into Lean or C frames;
//! - the `lean_inc` / `lean_dec` discipline behind every owned or
//!   borrowed Lean object handle (see [`obj`]).

pub(crate) mod init;
pub(crate) mod obj;
pub(crate) mod thread;

pub use init::LeanRuntime;

#[cfg(test)]
mod tests;
