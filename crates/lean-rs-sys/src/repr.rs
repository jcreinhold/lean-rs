//! Crate-private layout structs mirroring `lean.h:131–310`.
//!
//! Every Lean object subclass is mirrored here, regardless of whether the
//! crate's current inline helpers exercise it. Keeping the full set in one
//! file is the layout pin: a new helper for an existing object kind can
//! cast `*mut lean_object` straight to the matching repr without touching
//! the rest of the crate. `#[allow(dead_code)]` covers reprs whose
//! corresponding inline accessors live in higher layers (e.g. `lean-rs`).

#![allow(dead_code)]
// The `m_*` field-name prefix is intentional: it mirrors the C struct names
// from `lean.h:131–310` so the layout pin is reviewable side-by-side with
// the C header.
#![allow(clippy::struct_field_names)]
//!
//! These types are never re-exported. Inline mirrors elsewhere in the crate
//! cast `*mut lean_object` to `*mut LeanObjectRepr` (or one of the subclass
//! reprs) inside `unsafe { ... }` blocks. The cast is sound because:
//!
//! - `lean.h`'s layout is pinned at build time by `LEAN_HEADER_DIGEST` (see
//!   `build.rs`); a header byte-flip fails the build.
//! - Each cast is gated by a Lean ABI precondition recorded in the call
//!   site's `// SAFETY:` comment (e.g. "object's tag is `LeanString`" before
//!   casting to `LeanStringObjectRepr`).

use core::ffi::c_void;

use crate::types::lean_object;

/// Header common to every Lean heap object.
///
/// Mirrors `lean.h:131–136`:
/// ```c
/// typedef struct {
///     int      m_rc;          // signed 32-bit
///     unsigned m_cs_sz:16;    // 16-bit
///     unsigned m_other:8;     // 8-bit
///     unsigned m_tag:8;       // 8-bit
/// } lean_object;
/// ```
/// `m_rc` is stored as plain `i32` so the C-side layout is exact. Both
/// single- and multi-threaded fast paths materialize a safe `&AtomicI32`
/// via `AtomicI32::from_ptr(&raw mut m_rc)` at the call site (see
/// [`crate::refcount`]). All accesses are `Relaxed`-ordered, matching the
/// C source.
#[repr(C)]
pub(crate) struct LeanObjectRepr {
    pub(crate) m_rc: i32,
    pub(crate) m_cs_sz: u16,
    pub(crate) m_other: u8,
    pub(crate) m_tag: u8,
}

/// Mirrors `lean_ctor_object` (`lean.h:170–173`).
#[repr(C)]
pub(crate) struct LeanCtorObjectRepr {
    pub(crate) header: LeanObjectRepr,
    pub(crate) objs: [*mut lean_object; 0],
}

/// Mirrors `lean_array_object` (`lean.h:176–181`).
#[repr(C)]
pub(crate) struct LeanArrayObjectRepr {
    pub(crate) header: LeanObjectRepr,
    pub(crate) size: usize,
    pub(crate) capacity: usize,
    pub(crate) data: [*mut lean_object; 0],
}

/// Mirrors `lean_sarray_object` (`lean.h:184–189`).
#[repr(C)]
pub(crate) struct LeanSArrayObjectRepr {
    pub(crate) header: LeanObjectRepr,
    pub(crate) size: usize,
    pub(crate) capacity: usize,
    pub(crate) data: [u8; 0],
}

/// Mirrors `lean_string_object` (`lean.h:191–197`).
#[repr(C)]
pub(crate) struct LeanStringObjectRepr {
    pub(crate) header: LeanObjectRepr,
    pub(crate) size: usize,
    pub(crate) capacity: usize,
    pub(crate) length: usize,
    pub(crate) data: [u8; 0],
}

/// Mirrors `lean_closure_object` (`lean.h:199–205`).
#[repr(C)]
pub(crate) struct LeanClosureObjectRepr {
    pub(crate) header: LeanObjectRepr,
    pub(crate) fun: *mut c_void,
    pub(crate) arity: u16,
    pub(crate) num_fixed: u16,
    pub(crate) objs: [*mut lean_object; 0],
}

/// Mirrors `lean_ref_object` (`lean.h:207–210`).
#[repr(C)]
pub(crate) struct LeanRefObjectRepr {
    pub(crate) header: LeanObjectRepr,
    pub(crate) value: *mut lean_object,
}

/// Mirrors `lean_thunk_object` (`lean.h:212–216`).
///
/// `m_value` and `m_closure` are `_Atomic(lean_object *)` in C. Inline mirrors
/// that touch them use `AtomicPtr<lean_object>::from_ptr` at the call site.
#[repr(C)]
pub(crate) struct LeanThunkObjectRepr {
    pub(crate) header: LeanObjectRepr,
    pub(crate) value: *mut lean_object,
    pub(crate) closure: *mut lean_object,
}

/// Mirrors `lean_task_object` (`lean.h:284–288`). `m_imp` is opaque to us.
#[repr(C)]
pub(crate) struct LeanTaskObjectRepr {
    pub(crate) header: LeanObjectRepr,
    pub(crate) value: *mut lean_object,
    pub(crate) imp: *mut c_void,
}

/// Mirrors `lean_promise_object` (`lean.h:290–293`).
#[repr(C)]
pub(crate) struct LeanPromiseObjectRepr {
    pub(crate) header: LeanObjectRepr,
    pub(crate) result: *mut c_void,
}

/// Mirrors `lean_external_object` (`lean.h:306–310`).
#[repr(C)]
pub(crate) struct LeanExternalObjectRepr {
    pub(crate) header: LeanObjectRepr,
    pub(crate) class: *mut c_void,
    pub(crate) data: *mut c_void,
}

#[cfg(test)]
pub(crate) mod test_support {
    //! A pure-Rust stand-in for a Lean heap object header, used by the unit
    //! tests of the header/refcount fast paths in [`crate::object`] and
    //! [`crate::refcount`].
    //!
    //! The object lives in Rust-owned heap memory and is never handed to the
    //! Lean runtime, so these tests run under both native `cargo test` and
    //! `cargo miri test`—the latter validating the `AtomicI32::from_ptr`,
    //! `o.cast::<LeanObjectRepr>()`, and field-offset provenance that the FFI
    //! paths rely on. Callers must keep the refcount strictly positive on the
    //! decrement side so no test ever dispatches to `lean_dec_ref_cold`, which
    //! is an extern Miri cannot interpret.

    use super::LeanObjectRepr;
    use crate::types::lean_object;

    /// Owns a single heap-allocated [`LeanObjectRepr`] and hands out a
    /// `*mut lean_object` with read/write provenance over the whole header.
    pub(crate) struct MockObject {
        repr: *mut LeanObjectRepr,
    }

    impl MockObject {
        /// Allocate a synthetic header with the given refcount and tag. The
        /// `m_cs_sz` field is irrelevant to the fast paths and is zeroed.
        pub(crate) fn new(m_rc: i32, m_tag: u8, m_other: u8) -> Self {
            let boxed = Box::new(LeanObjectRepr {
                m_rc,
                m_cs_sz: 0,
                m_other,
                m_tag,
            });
            // `Box::into_raw` yields a pointer valid for reads and writes over
            // the allocation until we reclaim it in `Drop`; the refcount
            // mirrors mutate `m_rc` through a pointer derived from it.
            Self {
                repr: Box::into_raw(boxed),
            }
        }

        /// The object as the opaque pointer the `lean_*` helpers accept.
        pub(crate) fn ptr(&self) -> *mut lean_object {
            self.repr.cast::<lean_object>()
        }

        /// Read the live refcount field directly (test assertions only).
        pub(crate) fn rc(&self) -> i32 {
            // SAFETY: `repr` is a live, uniquely-owned allocation for the
            // lifetime of `self`; no concurrent access in single-threaded
            // tests.
            unsafe { (*self.repr).m_rc }
        }
    }

    impl Drop for MockObject {
        fn drop(&mut self) {
            // SAFETY: reclaim the `Box` leaked by `into_raw` in `new`; `repr`
            // has not been freed by any other path.
            unsafe {
                drop(Box::from_raw(self.repr));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::mem::{align_of, size_of};

    #[test]
    fn header_repr_matches_lean_h() {
        // lean.h:131–136—int + 16-bit + 8-bit + 8-bit = 8 bytes total.
        assert_eq!(size_of::<LeanObjectRepr>(), 8);
        assert_eq!(align_of::<LeanObjectRepr>(), 4);
    }

    #[test]
    fn ctor_repr_has_just_a_header() {
        // The `objs` flexible array is zero-sized; the struct should match
        // the header's footprint exactly (modulo whatever the C compiler
        // does—both targets we support are 64-bit, so no tail padding).
        assert_eq!(size_of::<LeanCtorObjectRepr>(), size_of::<LeanObjectRepr>());
    }

    #[test]
    fn array_repr_header_plus_two_words() {
        // header (8) + size (8) + capacity (8) on 64-bit.
        assert_eq!(
            size_of::<LeanArrayObjectRepr>(),
            size_of::<LeanObjectRepr>() + 2 * size_of::<usize>()
        );
    }

    #[test]
    fn string_repr_header_plus_three_words() {
        assert_eq!(
            size_of::<LeanStringObjectRepr>(),
            size_of::<LeanObjectRepr>() + 3 * size_of::<usize>()
        );
    }

    #[test]
    fn closure_repr_header_plus_fun_plus_arities() {
        // header (8) + fun (pointer, 8 on 64-bit) + arity (2) + num_fixed (2)
        // with tail padding to the pointer alignment for the flexible-array
        // member that follows.
        let unpadded = size_of::<LeanObjectRepr>() + size_of::<*mut core::ffi::c_void>() + 2 * size_of::<u16>();
        let pointer_align = align_of::<*mut core::ffi::c_void>();
        assert_eq!(
            size_of::<LeanClosureObjectRepr>(),
            unpadded.next_multiple_of(pointer_align)
        );
    }
}
