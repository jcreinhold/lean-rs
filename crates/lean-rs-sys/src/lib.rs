//! Raw FFI bindings for the Lean 4 C ABI.
//!
//! **Calling any function in this crate is `unsafe`.** Prefer the safe front
//! door in [`lean-rs`](https://docs.rs/lean-rs) — specifically its `host`,
//! `module`, and `error` modules — for almost all use cases. Raw FFI is the
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
//! fields. The crate's layout assumptions are pinned at build time by the
//! `LEAN_HEADER_DIGEST` check in `build.rs`.
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
//! See `crates/lean-rs-sys/README.md` for the supported Lean version range,
//! discovery rules followed by `build.rs`, and how to refresh
//! `EXPECTED_HEADER_DIGEST` when bumping Lean.

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
pub mod nat_int;
pub mod object;
pub mod refcount;
pub mod scalar;
pub mod string;
pub mod types;

pub(crate) mod repr;

pub use consts::{EXPECTED_HEADER_DIGEST, LEAN_HEADER_DIGEST, LEAN_HEADER_PATH, LEAN_VERSION};
pub use types::{b_lean_obj_arg, b_lean_obj_res, lean_obj_arg, lean_obj_res, lean_object, u_lean_obj_arg};

/// Names of `LEAN_EXPORT`'d symbols this crate's `extern "C"` blocks declare.
///
/// The `extern` blocks remain authoritative; tooling (`tests/linkage.rs`,
/// `lean-toolchain`'s re-export, version-compatibility docs) reads this list
/// to iterate over the surface without parsing source.
pub const REQUIRED_SYMBOLS: &[&str] = &[
    // init / runtime
    "lean_initialize",
    "lean_initialize_runtime_module",
    "lean_initialize_thread",
    "lean_finalize_thread",
    "lean_setup_args",
    "lean_init_task_manager",
    "lean_init_task_manager_using",
    "lean_finalize_task_manager",
    // refcount + marking
    "lean_dec_ref_cold",
    "lean_mark_mt",
    "lean_mark_persistent",
    // object / allocators
    "lean_alloc_object",
    "lean_free_object",
    "lean_object_byte_size",
    "lean_object_data_byte_size",
    // arrays
    "lean_array_mk",
    "lean_array_push",
    "lean_array_to_list",
    "lean_array_get_panic",
    "lean_array_set_panic",
    // strings
    "lean_mk_string",
    "lean_mk_string_unchecked",
    "lean_mk_string_from_bytes",
    "lean_mk_string_from_bytes_unchecked",
    "lean_mk_ascii_string_unchecked",
    "lean_string_push",
    "lean_string_append",
    "lean_string_mk",
    "lean_string_data",
    "lean_string_utf8_get",
    "lean_string_utf8_next",
    "lean_string_utf8_prev",
    "lean_string_utf8_set",
    "lean_string_utf8_extract",
    "lean_string_eq_cold",
    "lean_string_lt",
    "lean_string_hash",
    "lean_utf8_strlen",
    "lean_utf8_n_strlen",
    // Nat bignum dispatch
    "lean_nat_big_succ",
    "lean_nat_big_add",
    "lean_nat_big_sub",
    "lean_nat_big_mul",
    "lean_nat_big_div",
    "lean_nat_big_mod",
    "lean_nat_big_eq",
    "lean_nat_big_le",
    "lean_nat_big_lt",
    "lean_nat_overflow_mul",
    // Int bignum dispatch
    "lean_int_big_neg",
    "lean_int_big_add",
    "lean_int_big_sub",
    "lean_int_big_mul",
    "lean_int_big_div",
    "lean_int_big_mod",
    "lean_int_big_eq",
    "lean_int_big_le",
    "lean_int_big_lt",
    "lean_int_big_nonneg",
    // scalar widening
    "lean_big_usize_to_nat",
    "lean_big_uint64_to_nat",
    "lean_cstr_to_nat",
    "lean_big_int_to_int",
    "lean_big_size_t_to_int",
    "lean_big_int64_to_int",
    "lean_cstr_to_int",
    "lean_uint8_of_big_nat",
    // closure dispatch
    "lean_apply_1",
    "lean_apply_2",
    "lean_apply_3",
    "lean_apply_4",
    "lean_apply_5",
    "lean_apply_6",
    "lean_apply_7",
    "lean_apply_8",
    "lean_apply_9",
    "lean_apply_10",
    "lean_apply_11",
    "lean_apply_12",
    "lean_apply_13",
    "lean_apply_14",
    "lean_apply_15",
    "lean_apply_16",
    "lean_apply_n",
    "lean_apply_m",
    // IO
    "lean_io_mark_end_initialization",
    // external
    "lean_register_external_class",
];
