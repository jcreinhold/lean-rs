//! Wire protocol and shared value types for the `lean-rs` worker process
//! boundary.
//!
//! This crate is the single definition of every shape that flows between a
//! parent supervisor and a worker child process. It does not link
//! `libleanshared`, so it can be consumed by alternative transports, fuzz
//! harnesses, or recorders without pulling the Lean runtime. The parent
//! supervisor ([`lean-rs-worker-parent`](https://docs.rs/lean-rs-worker-parent))
//! and the child runtime
//! ([`lean-rs-worker-child`](https://docs.rs/lean-rs-worker-child)) both
//! depend on this crate and exchange framed messages defined here.
//!
//! ## Module map
//!
//! - [`types`] — `serde`-derived value types that appear in request and
//!   response bodies (elaboration options, kernel results, processed-file
//!   projections, capability metadata).
//! - [`protocol`] — the length-delimited frame codec, [`protocol::Frame`]
//!   envelope, [`protocol::Message`] message variants, and the
//!   [`protocol::Request`] / [`protocol::Response`] / [`protocol::Diagnostic`]
//!   / [`protocol::ProgressTick`] / [`protocol::DataRow`] /
//!   [`protocol::StreamSummary`] / [`protocol::FatalExit`] payload types, plus
//!   [`protocol::write_frame`] and [`protocol::read_frame`].
//!
//! ## Stability
//!
//! Every public enum, and every public struct that is not externally
//! constructed, is `#[non_exhaustive]` so additive evolution of the wire
//! format does not require a semver-major bump. Struct shapes with public
//! fields that downstream code legitimately constructs (currently [`protocol::DataRow`])
//! remain exhaustive; adding a field there is a breaking change.
//!
//! The hidden `worker_exports` module is an implementation table for the
//! closed worker capability operation shapes shared by the parent, child, and
//! test harness. It is not an extension registry for downstream callers.

#![forbid(unsafe_code)]

pub mod protocol;
pub mod types;
#[doc(hidden)]
pub mod worker_exports;

#[cfg(feature = "harness")]
pub mod harness;

/// Version of the `lean-rs-worker-protocol` crate, matching `Cargo.toml`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
