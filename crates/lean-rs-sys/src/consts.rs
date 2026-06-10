//! ABI constants for `lean-rs-sys`.
//!
//! The static `lean.h` layout constants (object tags, allocator ceilings) come
//! from the link-free `lean-rs-abi`.
//!
//! The live toolchain identity below is baked by this crate's `build.rs` from
//! the `lean.h` it linked against: `lean-rs-sys` links `libleanshared`, so it
//! always has a toolchain to probe (unlike `lean-rs-abi`, which is purely
//! static).

pub use lean_rs_abi::consts::*;

/// `LEAN_VERSION_STRING` from the `lean.h` this crate was built against.
pub const LEAN_VERSION: &str = env!("LEAN_VERSION");

/// Version string from the matched supported-toolchain entry.
///
/// Equal to [`LEAN_VERSION`] except when several releases share one `lean.h`
/// digest, in which case it is the first version listed for that entry.
pub const LEAN_RESOLVED_VERSION: &str = env!("LEAN_RESOLVED_VERSION");

/// Filesystem path to the `lean.h` this crate built against.
pub const LEAN_HEADER_PATH: &str = env!("LEAN_HEADER_PATH");

/// SHA-256 of the resolved `lean.h`, lowercase hex.
pub const LEAN_HEADER_DIGEST: &str = env!("LEAN_HEADER_DIGEST");
