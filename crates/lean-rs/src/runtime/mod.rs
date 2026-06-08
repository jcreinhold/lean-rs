//! Process-wide Lean runtime anchor.
//!
//! The lifetime-bound owned-object handle [`obj::Obj`] (with its
//! borrowed view [`obj::ObjRef`]) is `pub` here so the sibling
//! `lean-rs-host` crate can wrap them inside its host-defined handle
//! types. The init cell and thread-attach helpers stay `pub(crate)`—
//! callers reach them through [`LeanRuntime::init`] and
//! [`LeanThreadGuard::attach`] re-exported at the crate root.
//!
//! What this layer hides from the rest of the crate:
//!
//! - which raw `lean_rs_sys` symbols implement init and thread attach;
//! - the order in which those symbols must be called;
//! - the `OnceLock` cell that makes initialization process-once;
//! - the `catch_unwind` boundary that keeps Rust panics from unwinding
//!   into Lean or C frames;
//! - the `lean_inc` / `lean_dec` discipline behind every owned or
//!   borrowed Lean object handle.

pub(crate) mod init;
pub(crate) mod memory;
pub mod obj;
pub(crate) mod thread;

pub use init::LeanRuntime;
pub use thread::LeanThreadGuard;

#[cfg(test)]
mod tests;
