//! Raw FFI bindings for the Lean 4 C ABI.
//!
//! **Calling any function in this crate is `unsafe`.** Prefer the safe front
//! door in [`lean-rs`](https://docs.rs/lean-rs)—specifically its `host`,
//! `module`, and `error` modules—for almost all use cases. Raw FFI is the
//! escape hatch for embedders who need it.
//!
//! Raw functions inherit Lean's C ABI ownership rules:
//! - `lean_obj_arg` is **owned** (caller transfers a refcount).
//! - `b_lean_obj_arg` is **borrowed** (caller retains the refcount).
//! - `lean_obj_res` returns an **owned** reference.
//!
//! [`lean_object`] is intentionally opaque: it is zero-sized and `!Send +
//! !Sync + !Unpin`. Downstream code reaches refcount, tag, and payload state
//! only through this crate's `pub unsafe fn` helpers, never by reading
//! fields. The crate's layout assumptions are pinned at build time: `build.rs`
//! computes the SHA-256 of the active toolchain's `include/lean/lean.h` and
//! requires it to match one entry in [`SUPPORTED_TOOLCHAINS`]. The matched
//! entry's first `versions` field is then exposed at runtime via
//! [`consts::LEAN_RESOLVED_VERSION`].
//!
//! Layering:
//! - Inline mirrors of `lean.h`'s `static inline` helpers live alongside the
//!   `extern "C"` declarations for the matching category (e.g. refcount,
//!   string, array). Each `pub unsafe fn` carries a `# Safety` section, each
//!   `unsafe { ... }` block carries a `// SAFETY:` comment.
//! - A crate-private `repr` module defines the Lean object layout
//!   (`LeanObjectRepr` and friends). These types are intentionally not
//!   re-exported.
//!
//! See `crates/lean-rs-sys/README.md` for the supported Lean version window
//! and `docs/bump-toolchain.md` for the procedure to extend it.

#![allow(unsafe_code)]
#![allow(non_camel_case_types)]
#![allow(non_snake_case)]
#![allow(non_upper_case_globals)]

pub mod array;
pub mod closure;
pub mod consts;
pub mod ctor;
pub mod external;
pub mod init;
pub mod io;
pub mod memory;
pub mod nat_int;
pub mod object;
pub mod refcount;
pub mod scalar;
pub mod string;
pub mod supported;
pub mod types;

pub(crate) mod repr;

pub use lean_rs_abi::{
    LEAN_HEADER_DIGEST, LEAN_HEADER_PATH, LEAN_RESOLVED_VERSION, LEAN_VERSION, REQUIRED_SYMBOLS, SUPPORTED_TOOLCHAINS,
    SupportedToolchain, supported_by_digest, supported_for, symbol_in_all, symbol_present_in_window,
};
pub use types::{b_lean_obj_arg, b_lean_obj_res, lean_obj_arg, lean_obj_res, lean_object, u_lean_obj_arg};
