//! Elaboration and diagnostic policy defaults shared across the host stack and
//! the worker wire protocol.
//!
//! These constants live in `lean-toolchain` so the worker-protocol crate (which
//! sits below `lean-rs`/`lean-rs-host` in the dep graph) can reference the same
//! defaults the host stack uses, without a backward dep on `lean-rs-host` or a
//! mirror that drifts.

/// Default heartbeat ceiling — matches Lean's own `maxHeartbeats` default
/// at 4.29.1 (`Lean.Core.maxHeartbeats`).
pub const LEAN_HEARTBEAT_LIMIT_DEFAULT: u64 = 200_000;

/// Upper bound on the heartbeat ceiling. 1000× the default; values above
/// saturate at this ceiling so a runaway elaborator finishes in bounded
/// real time on every supported host.
pub const LEAN_HEARTBEAT_LIMIT_MAX: u64 = 200_000_000;

/// Default byte budget for the diagnostic collection returned per call
/// (64 KiB ≈ 16 default-bounded messages).
pub const LEAN_DIAGNOSTIC_BYTE_LIMIT_DEFAULT: usize = 64 * 1024;

/// Upper bound on the diagnostic byte budget (1 MiB).
pub const LEAN_DIAGNOSTIC_BYTE_LIMIT_MAX: usize = 1024 * 1024;
