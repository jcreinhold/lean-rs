//! Child-process runtime for the `lean-rs` worker boundary.
//!
//! This crate ships the binary that hosts a `lean-rs` runtime inside a
//! supervised child process and serves typed requests defined in
//! [`lean-rs-worker-protocol`](https://docs.rs/lean-rs-worker-protocol)
//! over stdin/stdout. Parent supervisors live in
//! [`lean-rs-worker-parent`](https://docs.rs/lean-rs-worker-parent).
//!
//! Most users do not depend on this crate directly; spawning the bundled
//! `lean-rs-worker-child` binary as a sibling executable is the usual path.
//! Applications that need a per-toolchain worker identity (so the binary
//! filename matches the host application) install their artifact and source policy
//! with [`run_worker_child_with_authority`].

mod authority;
pub use authority::{
    WorkerAuthority, WorkerManifestRead, WorkerSourceContext, install_worker_authority, run_worker_child_with_authority,
};
mod artifact_reads;
mod child;
mod host_artifacts;
#[cfg(unix)]
mod parent_watchdog;

/// Run the worker child process on stdin/stdout.
///
/// Applications that serve native requests supply their policy at process entry:
///
/// ```ignore
/// fn main() -> lean_rs::LeanResult<std::process::ExitCode> {
///     lean_rs_worker_child::run_worker_child_with_authority(application_policy())
/// }
/// ```
///
/// Parent processes should depend on `lean-rs-worker-parent` for the
/// supervisor surface (`LeanWorker`, `LeanWorkerCapabilityBuilder`,
/// `LeanWorkerPool`). This function is only the child-side binary entry
/// point.
pub fn run_worker_child_stdio() -> std::process::ExitCode {
    if let Some(exit) = host_artifacts::preparation_mode() {
        return exit;
    }
    child::run_stdio()
}
