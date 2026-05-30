//! Constructor objects and polymorphic boxing—Rust mirrors of
//! `lean.h:642–760` and the `lean_box_uint*` / `lean_box_float` family at
//! `lean.h:2811–2873`.
//!
//! Lean's polymorphic boxing for fixed-width unboxed values (`UInt32` on
//! 32-bit, `UInt64`, `USize`, `Float`, `Float32`) wraps the value in a
//! single-field constructor (`tag=0`, `num_objs=0`, scalar payload
//! sized for the value). The boxed form is the representation an
//! `Array UIntN` / `Option UIntN` / `Except E UIntN` field carries; the
//! direct unboxed form is reserved for argument and return positions in
//! exported Lean functions.
//!
//! Each helper here threads through the `lean_alloc_object` extern declared
//! in [`crate::object`] and writes header bytes via the crate-private
//! `LeanCtorObjectRepr` layout in `crate::repr`. The pinned
//! `EXPECTED_HEADER_DIGEST` guarantees the field offsets match the active
//! `lean.h`.

#![allow(clippy::inline_always)]

use core::mem::size_of;

use crate::consts::{LEAN_MAX_SMALL_OBJECT_SIZE, LEAN_OBJECT_SIZE_DELTA};
use crate::object::lean_alloc_object;
use crate::repr::{LeanCtorObjectRepr, LeanObjectRepr};
use crate::types::{b_lean_obj_arg, lean_obj_res, lean_object};

/// Write the single-threaded header (`m_rc=1`, `m_tag`, `m_other`) on a
/// freshly allocated object—Rust mirror of `lean_set_st_header`
/// (`lean.h:615–623`).
///
/// Constructor allocation uses Lean's small-object sizing convention:
/// `m_cs_sz` stores the aligned object byte size for live small objects.
/// The exported `lean_alloc_object` symbol does not initialise that byte
/// for callers that bypass Lean's inline `lean_alloc_ctor_memory`, so the
/// Rust mirror writes it here after allocation. Without this write,
/// `lean_object_byte_size` cannot recover the byte span of Rust-allocated
/// constructors, which in turn prevents safe scalar-tail bounds checks.
///
/// # Safety
///
/// `o` must point to a freshly allocated, otherwise-uninitialized Lean
/// heap object whose layout matches [`LeanObjectRepr`].
#[inline(always)]
unsafe fn set_st_header(o: *mut lean_object, tag: u8, other: u8, aligned_size: u16) {
    // SAFETY: precondition above; layout pinned by `EXPECTED_HEADER_DIGEST`.
    unsafe {
        let repr = o.cast::<LeanObjectRepr>();
        (*repr).m_rc = 1;
        (*repr).m_cs_sz = aligned_size;
        (*repr).m_tag = tag;
        (*repr).m_other = other;
    }
}

#[inline(always)]
fn align_small_object_size(sz: usize) -> usize {
    sz.strict_add(LEAN_OBJECT_SIZE_DELTA - 1) & !(LEAN_OBJECT_SIZE_DELTA - 1)
}

/// Number of object-pointer fields stored in a constructor (`lean.h:644`).
///
/// # Safety
///
/// `o` must be a borrowed Lean constructor object.
#[inline(always)]
pub unsafe fn lean_ctor_num_objs(o: b_lean_obj_arg) -> u8 {
    // SAFETY: precondition above; `m_other` is the num-objs field for ctor
    // objects (`lean.h:129`).
    unsafe { crate::object::lean_ptr_other(o) }
}

/// Pointer to the constructor's object-field storage (`lean.h:649–652`).
///
/// # Safety
///
/// `o` must be a borrowed Lean constructor object. The returned pointer is
/// valid for `lean_ctor_num_objs(o)` `*mut lean_object` slots.
#[inline(always)]
pub unsafe fn lean_ctor_obj_cptr(o: *mut lean_object) -> *mut *mut lean_object {
    // SAFETY: precondition above; flexible-array member at fixed offset.
    unsafe { (&raw mut (*o.cast::<LeanCtorObjectRepr>()).objs).cast::<*mut lean_object>() }
}

/// Pointer to the constructor's scalar payload storage
/// (`lean.h:654–657`). Sits immediately past the object-pointer slots.
///
/// # Safety
///
/// `o` must be a borrowed Lean constructor object whose scalar payload area
/// is at least `offset + size_of::<T>()` bytes wide for any `offset` the
/// caller subsequently passes to `lean_ctor_get_*` / `lean_ctor_set_*`.
#[inline(always)]
pub unsafe fn lean_ctor_scalar_cptr(o: *mut lean_object) -> *mut u8 {
    // SAFETY: precondition above; the scalar area starts one element past
    // the last `*mut lean_object` slot.
    unsafe {
        let num_objs = lean_ctor_num_objs(o) as usize;
        lean_ctor_obj_cptr(o).add(num_objs).cast::<u8>()
    }
}

/// Allocate a freshly initialized constructor object—Rust mirror of
/// `lean_alloc_ctor` (`lean.h:659–664`).
///
/// `tag` selects the constructor (`0..=LEAN_MAX_CTOR_TAG`), `num_objs`
/// names how many object-pointer fields it carries, and `scalar_sz` is the
/// byte width of the appended scalar payload. The returned object has
/// `m_rc=1`; the caller subsequently initialises every object field via
/// [`lean_ctor_obj_cptr`] writes and every scalar field via
/// `lean_ctor_set_*`.
///
/// # Safety
///
/// All three sizing parameters must fit `lean.h`'s
/// `LEAN_MAX_CTOR_TAG` / `LEAN_MAX_CTOR_FIELDS` / `LEAN_MAX_CTOR_SCALARS_SIZE`
/// ceilings. The caller must fully initialise every declared field before
/// passing the object to other Lean routines (notably the object-pointer
/// fields, which Lean's RC machinery will otherwise read as garbage).
///
/// # Panics
///
/// Panics if the computed small-object size exceeds Lean's
/// `LEAN_MAX_SMALL_OBJECT_SIZE`; this indicates a caller violated the
/// sizing preconditions above.
#[inline(always)]
pub unsafe fn lean_alloc_ctor(tag: u8, num_objs: u8, scalar_sz: usize) -> lean_obj_res {
    let sz = size_of::<LeanObjectRepr>()
        .strict_add(size_of::<*mut lean_object>().strict_mul(num_objs as usize))
        .strict_add(scalar_sz);
    let aligned_sz = align_small_object_size(sz);
    assert!(aligned_sz <= LEAN_MAX_SMALL_OBJECT_SIZE);
    #[allow(
        clippy::cast_possible_truncation,
        reason = "LEAN_MAX_SMALL_OBJECT_SIZE is below u16::MAX"
    )]
    let aligned_sz = aligned_sz as u16;
    // SAFETY: `lean_alloc_object` returns a non-null pointer to `sz` bytes of
    // uninitialised Lean-managed memory; we immediately install the
    // single-threaded header so any subsequent access through Lean's
    // predicates sees a well-formed object.
    unsafe {
        let o = lean_alloc_object(sz);
        set_st_header(o, tag, num_objs, aligned_sz);
        o
    }
}

/// Box a `u32` as a single-field constructor (`lean.h:2813–2823`).
///
/// On 64-bit hosts Lean's `UInt32` is already representable as a
/// scalar-tagged pointer via [`crate::object::lean_box`]; this helper is
/// the polymorphic-boxed form needed when a `UInt32` value lands in a
/// constructor field of an `Array UInt32` / `Option UInt32` / etc.
///
/// # Safety
///
/// Pure pointer arithmetic plus one `lean_alloc_object` call; no caller
/// preconditions.
#[inline(always)]
pub unsafe fn lean_box_uint32(v: u32) -> lean_obj_res {
    // SAFETY: ctor allocation is unconditional; we initialise the single
    // scalar payload before returning.
    unsafe {
        let o = lean_alloc_ctor(0, 0, size_of::<u32>());
        lean_ctor_set_uint32(o, 0, v);
        o
    }
}

/// Recover the `u32` payload from a constructor produced by
/// [`lean_box_uint32`] (`lean.h:2825–2833`).
///
/// # Safety
///
/// `o` must be a borrowed constructor object produced by
/// [`lean_box_uint32`] (or by Lean's compiler in a polymorphic position
/// holding a `UInt32`). On 64-bit hosts, scalar-tagged `o` is read
/// directly via [`crate::object::lean_unbox`] instead.
#[inline(always)]
pub unsafe fn lean_unbox_uint32(o: b_lean_obj_arg) -> u32 {
    // SAFETY: precondition above; layout pinned by build digest.
    unsafe { lean_ctor_get_uint32(o, 0) }
}

/// Box a `u64` as a single-field constructor (`lean.h:2835–2839`).
///
/// # Safety
///
/// Same as [`lean_box_uint32`].
#[inline(always)]
pub unsafe fn lean_box_uint64(v: u64) -> lean_obj_res {
    // SAFETY: ctor allocation is unconditional; payload initialised below.
    unsafe {
        let o = lean_alloc_ctor(0, 0, size_of::<u64>());
        lean_ctor_set_uint64(o, 0, v);
        o
    }
}

/// Recover the `u64` payload from a constructor produced by
/// [`lean_box_uint64`] (`lean.h:2841–2843`).
///
/// # Safety
///
/// `o` must be a borrowed constructor object produced by
/// [`lean_box_uint64`] (or by Lean's compiler in a polymorphic position
/// holding a `UInt64`).
#[inline(always)]
pub unsafe fn lean_unbox_uint64(o: b_lean_obj_arg) -> u64 {
    // SAFETY: precondition above.
    unsafe { lean_ctor_get_uint64(o, 0) }
}

/// Box a `usize` as a single-field constructor (`lean.h:2845–2849`).
///
/// # Safety
///
/// Same as [`lean_box_uint32`].
#[inline(always)]
pub unsafe fn lean_box_usize(v: usize) -> lean_obj_res {
    // SAFETY: ctor allocation is unconditional; payload initialised below.
    unsafe {
        let o = lean_alloc_ctor(0, 0, size_of::<usize>());
        lean_ctor_set_usize(o, 0, v);
        o
    }
}

/// Recover the `usize` payload from a constructor produced by
/// [`lean_box_usize`] (`lean.h:2851–2853`).
///
/// # Safety
///
/// `o` must be a borrowed constructor object produced by
/// [`lean_box_usize`] (or by Lean's compiler in a polymorphic position
/// holding a `USize`).
#[inline(always)]
pub unsafe fn lean_unbox_usize(o: b_lean_obj_arg) -> usize {
    // SAFETY: precondition above.
    unsafe { lean_ctor_get_usize(o, 0) }
}

/// Box an `f64` as a single-field constructor (`lean.h:2855–2859`).
///
/// # Safety
///
/// Same as [`lean_box_uint32`].
#[inline(always)]
pub unsafe fn lean_box_float(v: f64) -> lean_obj_res {
    // SAFETY: ctor allocation is unconditional; payload initialised below.
    unsafe {
        let o = lean_alloc_ctor(0, 0, size_of::<f64>());
        lean_ctor_set_float(o, 0, v);
        o
    }
}

/// Recover the `f64` payload from a constructor produced by
/// [`lean_box_float`] (`lean.h:2861–2863`).
///
/// # Safety
///
/// `o` must be a borrowed constructor object produced by [`lean_box_float`]
/// (or by Lean's compiler in a polymorphic position holding a `Float`).
#[inline(always)]
pub unsafe fn lean_unbox_float(o: b_lean_obj_arg) -> f64 {
    // SAFETY: precondition above.
    unsafe { lean_ctor_get_float(o, 0) }
}

/// Read a `usize` field stored after the object-pointer slots (`lean.h:692`).
///
/// # Safety
///
/// `o` must be a borrowed constructor object whose scalar payload includes
/// at least one `usize` at slot `i` (counted in `usize` units past the
/// object-pointer fields).
#[inline(always)]
pub unsafe fn lean_ctor_get_usize(o: b_lean_obj_arg, i: u8) -> usize {
    // SAFETY: precondition above. The scalar area starts at the first
    // object-slot pointer, which is `*mut lean_object`-aligned (8 bytes on
    // 64-bit, 4 on 32-bit)—sufficient for `usize`. `read_unaligned` is
    // used to mirror C's byte-pointer cast without invoking Rust's
    // strict-alignment requirement on plain pointer reads.
    unsafe { lean_ctor_obj_cptr(o).add(i as usize).cast::<usize>().read_unaligned() }
}

/// Read a `u8` field at byte `offset` within the scalar payload
/// (`lean.h:697–700`).
///
/// # Safety
///
/// `o` must be a borrowed constructor object whose scalar payload extends
/// at least `offset + 1` bytes past the object-pointer slots.
#[inline(always)]
pub unsafe fn lean_ctor_get_uint8(o: b_lean_obj_arg, offset: u32) -> u8 {
    // SAFETY: precondition above; mirrors C's pointer arithmetic verbatim.
    unsafe { *lean_ctor_scalar_cptr(o).add(offset as usize) }
}

/// Read a `u16` field at byte `offset` within the scalar payload
/// (`lean.h:702–705`).
///
/// # Safety
///
/// Same as [`lean_ctor_get_uint8`]; the caller is expected to use a
/// naturally aligned `offset`, but `read_unaligned` makes the call sound
/// even if alignment is off.
#[inline(always)]
pub unsafe fn lean_ctor_get_uint16(o: b_lean_obj_arg, offset: u32) -> u16 {
    // SAFETY: precondition above.
    unsafe {
        lean_ctor_scalar_cptr(o)
            .add(offset as usize)
            .cast::<u16>()
            .read_unaligned()
    }
}

/// Read a `u32` field at byte `offset` within the scalar payload
/// (`lean.h:707–710`).
///
/// # Safety
///
/// Same as [`lean_ctor_get_uint16`].
#[inline(always)]
pub unsafe fn lean_ctor_get_uint32(o: b_lean_obj_arg, offset: u32) -> u32 {
    // SAFETY: precondition above.
    unsafe {
        lean_ctor_scalar_cptr(o)
            .add(offset as usize)
            .cast::<u32>()
            .read_unaligned()
    }
}

/// Read a `u64` field at byte `offset` within the scalar payload
/// (`lean.h:712–715`).
///
/// # Safety
///
/// Same as [`lean_ctor_get_uint16`].
#[inline(always)]
pub unsafe fn lean_ctor_get_uint64(o: b_lean_obj_arg, offset: u32) -> u64 {
    // SAFETY: precondition above.
    unsafe {
        lean_ctor_scalar_cptr(o)
            .add(offset as usize)
            .cast::<u64>()
            .read_unaligned()
    }
}

/// Read an `f64` field at byte `offset` within the scalar payload
/// (`lean.h:717–720`).
///
/// # Safety
///
/// Same as [`lean_ctor_get_uint64`].
#[inline(always)]
pub unsafe fn lean_ctor_get_float(o: b_lean_obj_arg, offset: u32) -> f64 {
    // SAFETY: precondition above.
    unsafe {
        lean_ctor_scalar_cptr(o)
            .add(offset as usize)
            .cast::<f64>()
            .read_unaligned()
    }
}

/// Write a `usize` field at slot `i` (counted in `usize` units past the
/// object-pointer slots)—mirror of `lean.h:727–730`.
///
/// # Safety
///
/// `o` must be a borrowed Lean constructor object whose scalar payload
/// includes at least one `usize` at slot `i`.
#[inline(always)]
pub unsafe fn lean_ctor_set_usize(o: b_lean_obj_arg, i: u8, v: usize) {
    // SAFETY: precondition above; pointer-aligned write through
    // `write_unaligned` to match the read-side helper.
    unsafe { lean_ctor_obj_cptr(o).add(i as usize).cast::<usize>().write_unaligned(v) }
}

/// Write a `u8` field at byte `offset` within the scalar payload
/// (`lean.h:732–735`).
///
/// # Safety
///
/// `o` must be a borrowed Lean constructor object whose scalar payload
/// extends at least `offset + 1` bytes past the object-pointer slots.
#[inline(always)]
pub unsafe fn lean_ctor_set_uint8(o: b_lean_obj_arg, offset: u32, v: u8) {
    // SAFETY: precondition above.
    unsafe { *lean_ctor_scalar_cptr(o).add(offset as usize) = v }
}

/// Write a `u16` field at byte `offset` within the scalar payload
/// (`lean.h:737–740`).
///
/// # Safety
///
/// Same as [`lean_ctor_set_uint8`].
#[inline(always)]
pub unsafe fn lean_ctor_set_uint16(o: b_lean_obj_arg, offset: u32, v: u16) {
    // SAFETY: precondition above.
    unsafe {
        lean_ctor_scalar_cptr(o)
            .add(offset as usize)
            .cast::<u16>()
            .write_unaligned(v);
    }
}

/// Write a `u32` field at byte `offset` within the scalar payload
/// (`lean.h:742–745`).
///
/// # Safety
///
/// Same as [`lean_ctor_set_uint16`].
#[inline(always)]
pub unsafe fn lean_ctor_set_uint32(o: b_lean_obj_arg, offset: u32, v: u32) {
    // SAFETY: precondition above.
    unsafe {
        lean_ctor_scalar_cptr(o)
            .add(offset as usize)
            .cast::<u32>()
            .write_unaligned(v);
    }
}

/// Write a `u64` field at byte `offset` within the scalar payload
/// (`lean.h:747–750`).
///
/// # Safety
///
/// Same as [`lean_ctor_set_uint16`].
#[inline(always)]
pub unsafe fn lean_ctor_set_uint64(o: b_lean_obj_arg, offset: u32, v: u64) {
    // SAFETY: precondition above.
    unsafe {
        lean_ctor_scalar_cptr(o)
            .add(offset as usize)
            .cast::<u64>()
            .write_unaligned(v);
    }
}

/// Write an `f64` field at byte `offset` within the scalar payload
/// (`lean.h:752–755`).
///
/// # Safety
///
/// Same as [`lean_ctor_set_uint64`].
#[inline(always)]
pub unsafe fn lean_ctor_set_float(o: b_lean_obj_arg, offset: u32, v: f64) {
    // SAFETY: precondition above.
    unsafe {
        lean_ctor_scalar_cptr(o)
            .add(offset as usize)
            .cast::<f64>()
            .write_unaligned(v);
    }
}

#[cfg(test)]
mod tests {
    //! Round-trip tests over the polymorphic-boxed scalar helpers.
    //!
    //! These touch real Lean allocations; they require `libleanshared` to be
    //! discoverable at run time (the workspace's `build.rs` files bake an
    //! rpath into the test binary).

    #![allow(clippy::expect_used, clippy::float_cmp)]

    use super::{lean_box_float, lean_box_uint32, lean_box_uint64, lean_box_usize};
    use super::{lean_unbox_float, lean_unbox_uint32, lean_unbox_uint64, lean_unbox_usize};
    use crate::init::{lean_initialize, lean_initialize_runtime_module};
    use crate::io::lean_io_mark_end_initialization;
    use crate::object::lean_box;
    use crate::refcount::lean_dec;

    /// Bring the Lean runtime up exactly once for this crate's tests. The
    /// safe `LeanRuntime` lives in `lean-rs`, which is downstream; here we
    /// open-code the same one-shot pattern with a local `OnceLock` so this
    /// crate's tests stay self-contained.
    fn ensure_runtime() {
        use std::sync::OnceLock;
        static INIT: OnceLock<()> = OnceLock::new();
        INIT.get_or_init(|| {
            // SAFETY: standard Lean init sequence (`lean.h` "How to use").
            unsafe {
                lean_initialize_runtime_module();
                lean_initialize();
                lean_io_mark_end_initialization();
            }
        });
    }

    #[test]
    #[cfg_attr(miri, ignore = "executes libleanshared; Miri cannot interpret the Lean C runtime")]
    fn box_unbox_uint64_round_trips() {
        ensure_runtime();
        for v in [0_u64, 1, u64::from(u32::MAX), u64::MAX] {
            // SAFETY: `lean_box_uint64` produces an owned ctor; we read it
            // back through `lean_unbox_uint64` and release via `lean_dec`.
            unsafe {
                let o = lean_box_uint64(v);
                assert_eq!(lean_unbox_uint64(o), v);
                lean_dec(o);
            }
        }
    }

    #[test]
    #[cfg_attr(miri, ignore = "executes libleanshared; Miri cannot interpret the Lean C runtime")]
    fn box_unbox_usize_round_trips() {
        ensure_runtime();
        for v in [0_usize, 1, usize::MAX] {
            // SAFETY: same ownership contract as `box_unbox_uint64_round_trips`.
            unsafe {
                let o = lean_box_usize(v);
                assert_eq!(lean_unbox_usize(o), v);
                lean_dec(o);
            }
        }
    }

    #[test]
    #[cfg_attr(miri, ignore = "executes libleanshared; Miri cannot interpret the Lean C runtime")]
    fn box_unbox_uint32_round_trips() {
        ensure_runtime();
        for v in [0_u32, 1, u32::MAX] {
            // SAFETY: same ownership contract as `box_unbox_uint64_round_trips`.
            unsafe {
                let o = lean_box_uint32(v);
                assert_eq!(lean_unbox_uint32(o), v);
                lean_dec(o);
            }
        }
    }

    #[test]
    #[cfg_attr(miri, ignore = "executes libleanshared; Miri cannot interpret the Lean C runtime")]
    fn box_unbox_float_round_trips() {
        ensure_runtime();
        for v in [0.0_f64, -1.5, core::f64::consts::PI, f64::INFINITY] {
            // SAFETY: same ownership contract as `box_unbox_uint64_round_trips`.
            unsafe {
                let o = lean_box_float(v);
                assert_eq!(lean_unbox_float(o), v);
                lean_dec(o);
            }
        }
        // NaN is a separate assertion: `==` is false against itself.
        // SAFETY: same ownership contract as above.
        unsafe {
            let o = lean_box_float(f64::NAN);
            assert!(lean_unbox_float(o).is_nan());
            lean_dec(o);
        }
    }

    #[test]
    #[cfg_attr(miri, ignore = "executes libleanshared; Miri cannot interpret the Lean C runtime")]
    fn alloc_sarray_round_trips_payload_bytes() {
        ensure_runtime();
        use crate::array::{
            lean_alloc_sarray, lean_sarray_capacity, lean_sarray_cptr, lean_sarray_elem_size, lean_sarray_size,
        };

        let bytes: &[u8] = b"hello\0world";
        // SAFETY: allocate a one-byte-element sarray sized to `bytes`, copy
        // into the storage, then read the header + payload back out.
        unsafe {
            let o = lean_alloc_sarray(1, bytes.len(), bytes.len());
            core::ptr::copy_nonoverlapping(bytes.as_ptr(), lean_sarray_cptr(o), bytes.len());

            assert_eq!(lean_sarray_elem_size(o), 1);
            assert_eq!(lean_sarray_size(o), bytes.len());
            assert_eq!(lean_sarray_capacity(o), bytes.len());

            let view = core::slice::from_raw_parts(lean_sarray_cptr(o), lean_sarray_size(o));
            assert_eq!(view, bytes);

            lean_dec(o);
        }
    }

    #[test]
    #[cfg_attr(miri, ignore = "executes libleanshared; Miri cannot interpret the Lean C runtime")]
    fn alloc_sarray_empty_is_valid() {
        ensure_runtime();
        use crate::array::{lean_alloc_sarray, lean_sarray_size};
        // SAFETY: zero-length sarray; allocation succeeds and size reads back
        // as zero.
        unsafe {
            let o = lean_alloc_sarray(1, 0, 0);
            assert_eq!(lean_sarray_size(o), 0);
            lean_dec(o);
        }
    }

    #[test]
    #[cfg_attr(miri, ignore = "executes libleanshared; Miri cannot interpret the Lean C runtime")]
    fn alloc_array_round_trips_object_slots() {
        ensure_runtime();
        use crate::array::{
            lean_alloc_array, lean_array_capacity, lean_array_get_core, lean_array_set_core, lean_array_size,
        };
        use crate::object::{lean_box, lean_is_array, lean_unbox};

        // SAFETY: build an object array of three scalar elements, read each
        // slot back via `lean_array_get_core`, then release. Scalar
        // elements skip refcount churn so the test isolates the array
        // allocator and slot-write path.
        unsafe {
            let o = lean_alloc_array(3, 3);
            assert!(lean_is_array(o));
            assert_eq!(lean_array_size(o), 3);
            assert_eq!(lean_array_capacity(o), 3);

            lean_array_set_core(o, 0, lean_box(10));
            lean_array_set_core(o, 1, lean_box(20));
            lean_array_set_core(o, 2, lean_box(30));

            assert_eq!(lean_unbox(lean_array_get_core(o, 0)), 10);
            assert_eq!(lean_unbox(lean_array_get_core(o, 1)), 20);
            assert_eq!(lean_unbox(lean_array_get_core(o, 2)), 30);

            lean_dec(o);
        }
    }

    #[test]
    #[cfg_attr(miri, ignore = "executes libleanshared; Miri cannot interpret the Lean C runtime")]
    fn alloc_array_empty_is_valid() {
        ensure_runtime();
        use crate::array::{lean_alloc_array, lean_array_capacity, lean_array_size};
        use crate::object::lean_is_array;

        // SAFETY: zero-length object array; allocation succeeds and the
        // size/capacity header reads back as zero. No element slots to
        // initialise.
        unsafe {
            let o = lean_alloc_array(0, 0);
            assert!(lean_is_array(o));
            assert_eq!(lean_array_size(o), 0);
            assert_eq!(lean_array_capacity(o), 0);
            lean_dec(o);
        }
    }

    #[test]
    #[cfg_attr(miri, ignore = "executes libleanshared; Miri cannot interpret the Lean C runtime")]
    fn scalar_box_unbox_remains_inline_for_small_nat() {
        // Sanity: the existing scalar `lean_box` / `lean_unbox` from
        // `crate::object` is a distinct path that must not interact with
        // the ctor-box helpers added here.
        // SAFETY: scalar-tagged pointer arithmetic only.
        unsafe {
            let o = lean_box(42);
            assert_eq!(crate::object::lean_unbox(o), 42);
            lean_dec(o);
        }
    }
}
