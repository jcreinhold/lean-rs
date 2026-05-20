//! Worker-process boundary for `lean-rs` host workloads.
//!
//! This crate is intentionally internal at prompt 56. It proves a child
//! runner and a versioned framed protocol before prompt 57 adds the public
//! supervisor API. The protocol module is private; callers should not learn
//! frame bytes, child pipe handling, or restart bookkeeping.

mod child;
mod protocol;

/// Run the prompt-56 child process on stdin/stdout.
///
/// This entry point exists for the `lean-rs-worker-child` binary. It is not
/// the public worker API; prompt 57 will add a supervisor surface over this
/// child runner.
#[doc(hidden)]
pub fn __run_child_stdio() -> std::process::ExitCode {
    child::run_stdio()
}

#[doc(hidden)]
pub mod __test_support;
