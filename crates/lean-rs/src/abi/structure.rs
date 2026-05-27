//! Constructor-object plumbing for product and sum structures.
//!
//! The "structure pattern" lives at two primitives, hand-called at each
//! struct boundary. There is no per-struct trait, no derive, no
//! procedural macro: callers compose [`alloc_ctor_with_objects`] and
//! [`take_ctor_objects`] field by field and let the per-field
//! [`super::traits::IntoLean`] / [`super::traits::TryFromLean`] impls
//! do the actual type marshalling.
//!
//! The module is the only place in `abi` that knows how
//! [`lean_alloc_ctor`], [`lean_ctor_obj_cptr`], and the constructor's
//! `tag`/`num_objs` invariants line up; container modules
//! (`crate::abi::option`, `crate::abi::except`) and downstream
//! handlers ship through these primitives instead of repeating the
//! pointer arithmetic. That keeps a single audited copy of the
//! ctor-allocation rules and centralises the `lean_inc`/`lean_dec`
//! reasoning.
//!
//! ### Lifetime and refcount invariants
//!
//! - [`alloc_ctor_with_objects`] consumes the input array of `Obj<'lean>`
//!   handles. Each handle's owned refcount is transferred — via
//!   [`Obj::into_raw`] — into the freshly allocated constructor's
//!   object-slot, so the new parent owns exactly one count per field plus
//!   its own header count. No `Obj::clone` (which would `lean_inc`) runs
//!   on the input path.
//! - [`take_ctor_objects`] reads each object slot once, calls `lean_inc`
//!   on the field, and wraps the bumped pointer in a fresh `Obj<'lean>`.
//!   The parent `Obj` is then dropped; its `lean_dec` walks back through
//!   the original per-field counts, leaving each returned handle with the
//!   same effective ownership the parent had given that field.
//! - [`ObjView`] / [`CtorView`] are borrow-only readers for constructor
//!   tags, scalar-tagged nullary constructors, and scalar-tail fields.
//!   They never expose raw pointers to callers and never touch the
//!   refcount.

// SAFETY DOC: every `unsafe { ... }` block in this file carries its own
// `// SAFETY:` comment naming the invariant; the blanket allow keeps the
// unsafe surface inside the smallest scope that compiles, per
// `docs/architecture/01-safety-model.md`.
#![allow(unsafe_code)]

use core::mem::MaybeUninit;

use lean_rs_sys::ctor::{
    lean_alloc_ctor, lean_ctor_get_uint8, lean_ctor_get_uint64, lean_ctor_num_objs, lean_ctor_obj_cptr,
    lean_ctor_scalar_cptr,
};
use lean_rs_sys::object::{lean_is_ctor, lean_is_scalar, lean_obj_tag, lean_object_data_byte_size};
use lean_rs_sys::refcount::lean_inc;

use crate::abi::traits::conversion_error;
use crate::error::LeanResult;
use crate::runtime::LeanRuntime;
use crate::runtime::obj::Obj;

/// Borrowed, allocation-free inspection view over an existing Lean object.
///
/// This is the host-facing boundary for Lean's scalar/nullary and
/// constructor-object representation. Callers can ask whether a value is
/// scalar-tagged, read a scalar constructor tag, or narrow to
/// [`CtorView`] before reading constructor header and scalar-tail fields.
/// The view never transfers ownership and never exposes the underlying
/// `lean_object*`.
pub struct ObjView<'lean, 'a> {
    obj: &'a Obj<'lean>,
}

/// Borrowed view of a heap-allocated Lean constructor.
///
/// Constructed only after [`ObjView::ctor`] has verified that the source
/// object is a constructor. The cached header facts make repeated reads
/// cheap and keep scalar-tail bounds checks explicit at each offset.
#[derive(Copy, Clone)]
pub struct CtorView<'lean, 'a> {
    obj: &'a Obj<'lean>,
    tag: u8,
    num_object_fields: usize,
    scalar_tail_size: usize,
}

/// Build a borrowed view over `obj`.
#[inline]
#[must_use]
pub fn view<'lean, 'a>(obj: &'a Obj<'lean>) -> ObjView<'lean, 'a> {
    ObjView { obj }
}

impl<'lean, 'a> ObjView<'lean, 'a> {
    /// Whether the object is Lean's scalar-tagged pointer form.
    #[inline]
    #[must_use]
    pub fn is_scalar(&self) -> bool {
        let ptr = self.obj.as_raw_borrowed();
        // SAFETY: pure pointer-bit inspection.
        unsafe { lean_is_scalar(ptr) }
    }

    /// Read the payload of a scalar-tagged object.
    ///
    /// This is the representation Lean uses for nullary-only inductive
    /// values and for some small primitive values (`Nat`, `Bool`, `Unit`
    /// in boxed positions). The caller supplies `label` only for the
    /// error message; it is not touched on the success path.
    ///
    /// # Errors
    ///
    /// Returns a conversion error if the object is heap-allocated.
    #[inline]
    pub fn scalar_payload(&self, label: &str) -> LeanResult<usize> {
        let ptr = self.obj.as_raw_borrowed();
        // SAFETY: pure pointer-bit inspection.
        if unsafe { lean_is_scalar(ptr) } {
            // SAFETY: scalar branch verified above.
            Ok(unsafe { lean_rs_sys::object::lean_unbox(ptr) })
        } else {
            // SAFETY: non-scalar branch; object tag is valid for any live
            // heap object held by `Obj`.
            let found_tag = unsafe { lean_obj_tag(ptr) };
            Err(conversion_error(format!(
                "expected Lean {label} scalar-tagged object, found heap object with tag {found_tag}"
            )))
        }
    }

    /// Read a sum-constructor tag encoded either as a scalar nullary tag
    /// or as a heap constructor tag.
    ///
    /// This matches Lean's mixed-inductive ABI rule: nullary constructors
    /// can be scalar-tagged, while constructors with fields are heap
    /// ctors. The returned tag is the Lean declaration-order constructor
    /// index.
    ///
    /// # Errors
    ///
    /// Returns a conversion error if the object is heap-allocated but is
    /// not a constructor, or if a scalar payload does not fit in `u8`.
    #[inline]
    pub fn sum_tag(&self) -> LeanResult<u8> {
        let ptr = self.obj.as_raw_borrowed();
        // SAFETY: pure pointer-bit inspection.
        if unsafe { lean_is_scalar(ptr) } {
            // SAFETY: scalar branch verified above.
            let tag = unsafe { lean_rs_sys::object::lean_unbox(ptr) };
            return u8::try_from(tag)
                .map_err(|_| conversion_error(format!("Lean scalar constructor tag {tag} does not fit in u8")));
        }
        self.ctor().map(|ctor| ctor.tag())
    }

    /// Narrow this object to a heap-constructor view.
    ///
    /// # Errors
    ///
    /// Returns a conversion error if the object is scalar-tagged or is a
    /// non-constructor heap object.
    #[inline]
    pub fn ctor(&self) -> LeanResult<CtorView<'lean, 'a>> {
        CtorView::new(self.obj)
    }

    /// Narrow this object to a constructor with the expected tag and
    /// object-slot count.
    ///
    /// This is the common generated-result shape check. It validates the
    /// constructor tag and object-field arity before any caller reads
    /// scalar-tail values or consumes fields through [`take_ctor_objects`].
    ///
    /// # Errors
    ///
    /// Returns a conversion error if the object is not a constructor, or
    /// if the constructor tag or object-field count differs.
    #[inline]
    pub fn ctor_shape(
        &self,
        expected_tag: u8,
        expected_num_object_fields: usize,
        label: &str,
    ) -> LeanResult<CtorView<'lean, 'a>> {
        self.ctor()?
            .require_shape(expected_tag, expected_num_object_fields, label)
    }
}

impl<'lean, 'a> CtorView<'lean, 'a> {
    #[inline]
    fn new(obj: &'a Obj<'lean>) -> LeanResult<Self> {
        let ptr = obj.as_raw_borrowed();
        // SAFETY: pure pointer-bit inspection.
        if unsafe { lean_is_scalar(ptr) } {
            return Err(conversion_error(
                "expected Lean constructor, found scalar-tagged object",
            ));
        }
        // SAFETY: non-scalar; ctor predicate inspects the header tag only.
        if !unsafe { lean_is_ctor(ptr) } {
            // SAFETY: same branch.
            let found_tag = unsafe { lean_obj_tag(ptr) };
            return Err(conversion_error(format!(
                "expected Lean constructor, found object with tag {found_tag}"
            )));
        }
        // SAFETY: ctor object — its tag fits a `u8` per Lean's
        // `LEAN_MAX_CTOR_TAG` ceiling, and `m_other` holds the object
        // field count for ctors.
        let tag = unsafe { lean_obj_tag(ptr) };
        let num_object_fields = unsafe { lean_ctor_num_objs(ptr) } as usize;
        #[allow(
            clippy::cast_possible_truncation,
            reason = "ctor tag is bounded by LEAN_MAX_CTOR_TAG"
        )]
        let tag = tag as u8;
        let scalar_tail_size = ctor_scalar_tail_size(ptr);
        Ok(Self {
            obj,
            tag,
            num_object_fields,
            scalar_tail_size,
        })
    }

    /// Constructor tag in Lean declaration order.
    #[inline]
    #[must_use]
    pub fn tag(&self) -> u8 {
        self.tag
    }

    #[inline]
    fn require_tag(self, expected_tag: u8, label: &str) -> LeanResult<Self> {
        if self.tag == expected_tag {
            Ok(self)
        } else {
            Err(conversion_error(format!(
                "expected Lean {label} ctor (tag {expected_tag}), found tag {}",
                self.tag
            )))
        }
    }

    #[inline]
    fn require_shape(self, expected_tag: u8, expected_num_object_fields: usize, label: &str) -> LeanResult<Self> {
        let this = self.require_tag(expected_tag, label)?;
        if this.num_object_fields == expected_num_object_fields {
            Ok(this)
        } else {
            Err(conversion_error(format!(
                "expected Lean {label} ctor with {expected_num_object_fields} object field(s), found {}",
                this.num_object_fields
            )))
        }
    }

    /// Read a `UInt8` field from the constructor scalar tail at byte
    /// `offset`.
    ///
    /// # Errors
    ///
    /// Returns a conversion error if the requested byte range is outside
    /// the scalar tail.
    #[inline]
    pub fn uint8(&self, offset: u32, label: &str) -> LeanResult<u8> {
        self.require_scalar_tail(offset, 1, label)?;
        let ptr = self.obj.as_raw_borrowed();
        // SAFETY: constructor kind was validated by `CtorView::new`, and
        // the explicit bounds check above proves `offset..offset+1` lies
        // inside the scalar tail.
        Ok(unsafe { lean_ctor_get_uint8(ptr, offset) })
    }

    /// Read a `UInt64` field from the constructor scalar tail at byte
    /// `offset`.
    ///
    /// # Errors
    ///
    /// Returns a conversion error if the requested byte range is outside
    /// the scalar tail.
    #[inline]
    pub fn uint64(&self, offset: u32, label: &str) -> LeanResult<u64> {
        self.require_scalar_tail(offset, core::mem::size_of::<u64>(), label)?;
        let ptr = self.obj.as_raw_borrowed();
        // SAFETY: constructor kind was validated by `CtorView::new`, and
        // the explicit bounds check above proves `offset..offset+8` lies
        // inside the scalar tail. The sys helper performs an unaligned
        // read, matching Lean's C accessor.
        Ok(unsafe { lean_ctor_get_uint64(ptr, offset) })
    }

    /// Decode a `Bool`-encoded scalar-tail byte.
    ///
    /// # Errors
    ///
    /// Returns a conversion error if the byte is outside the scalar tail
    /// or is not `0` / `1`.
    #[inline]
    pub fn bool(&self, offset: u32, label: &str) -> LeanResult<bool> {
        match self.uint8(offset, label)? {
            0 => Ok(false),
            1 => Ok(true),
            other => Err(conversion_error(format!(
                "Lean {label} byte {other} is not in {{0, 1}}"
            ))),
        }
    }

    #[inline]
    fn require_scalar_tail(&self, offset: u32, width: usize, label: &str) -> LeanResult<()> {
        let start = offset as usize;
        let Some(end) = start.checked_add(width) else {
            return Err(conversion_error(format!(
                "Lean {label} scalar-tail read at offset {offset} overflows usize"
            )));
        };
        if end <= self.scalar_tail_size {
            Ok(())
        } else {
            Err(conversion_error(format!(
                "Lean {label} scalar-tail read at offset {offset} for {width} byte(s) exceeds scalar tail size {}",
                self.scalar_tail_size
            )))
        }
    }
}

/// Allocate a freshly-initialised constructor with `N` object-pointer
/// fields and no scalar payload.
///
/// `tag` is the inductive constructor index (Lean's declaration order:
/// `Option.none` = 0, `Option.some` = 1, `Except.error` = 0, `Except.ok`
/// = 1, …). Each entry of `objects` is moved into its slot via
/// [`Obj::into_raw`], so the returned [`Obj`] owns the only live refcount
/// per field plus its own header count. The const-generic `N` matches the
/// number of object-pointer slots the Lean inductive declares, which
/// keeps the call site self-documenting and lets the compiler refuse
/// arity mismatches.
///
/// # Panics
///
/// Panics only via `lean_alloc_ctor`'s `strict_*` arithmetic overflow
/// guard — unreachable for the constructor shapes Lean emits
/// (`LEAN_MAX_CTOR_FIELDS` = 256).
pub fn alloc_ctor_with_objects<'lean, const N: usize>(
    runtime: &'lean LeanRuntime,
    tag: u8,
    objects: [Obj<'lean>; N],
) -> Obj<'lean> {
    // SAFETY: `lean_alloc_ctor` returns a fresh ctor with refcount 1 and
    // `N` uninitialised object slots; we write each `Obj::into_raw` into
    // its slot before the object escapes, satisfying the
    // "fully initialise every declared field" obligation.
    unsafe {
        // The `num_objs` parameter is `u8`; assert at compile time that
        // `N` fits, matching `lean.h`'s `LEAN_MAX_CTOR_FIELDS` ceiling
        // (the const block evaluates at type-checking time).
        const { assert!(N <= u8::MAX as usize, "ctor arity exceeds Lean's u8 num_objs field") };
        let raw = lean_alloc_ctor(tag, N as u8, 0);
        let slots = lean_ctor_obj_cptr(raw);
        for (i, field) in objects.into_iter().enumerate() {
            *slots.add(i) = field.into_raw();
        }
        Obj::from_owned_raw(runtime, raw)
    }
}

/// Validate that `obj` is a constructor with `expected_tag` and exactly
/// `N` object-pointer fields, then return the `N` owned field handles.
///
/// Each returned [`Obj`] carries one refcount: [`lean_inc`] is called on
/// the slot pointer before wrapping it. The parent `obj` is consumed and
/// its [`Drop`] runs the matching `lean_dec` (which decrements each field
/// once more — balancing the `lean_inc`s and leaving the returned handles
/// with the same effective ownership the parent originally held).
///
/// `label` is embedded in the diagnostic on failure so callers see
/// `"expected Lean Option::some ctor (tag 1, num_objs 1), …"` rather
/// than an anonymous "wrong ctor".
///
/// # Errors
///
/// Returns [`HostStage::Conversion`](crate::error::HostStage::Conversion)
/// if `obj` is scalar-tagged, has a non-constructor heap tag, has a
/// different tag from `expected_tag`, or carries a different
/// object-slot count from `N`.
pub fn take_ctor_objects<'lean, const N: usize>(
    obj: Obj<'lean>,
    expected_tag: u8,
    label: &str,
) -> LeanResult<[Obj<'lean>; N]> {
    require_ctor_shape(&obj, expected_tag, N, label)?;
    let runtime = obj.runtime();
    let ptr = obj.as_raw_borrowed();
    // SAFETY: shape validated above; `lean_ctor_obj_cptr` returns a
    // pointer valid for `N` slots, each holding a live owned
    // `*mut lean_object` (well-formed Lean ctor invariant).
    let slots = unsafe { lean_ctor_obj_cptr(ptr) };
    let mut out: [MaybeUninit<Obj<'lean>>; N] = [const { MaybeUninit::uninit() }; N];
    for (i, slot) in out.iter_mut().enumerate() {
        // SAFETY: index in `0..N` is in-bounds per the shape check; the
        // slot read is a borrowed view, then `lean_inc` bumps the refcount
        // so the wrapped `Obj` owns its own count independent of the
        // parent.
        unsafe {
            let field_ptr = *slots.add(i);
            lean_inc(field_ptr);
            slot.write(Obj::from_owned_raw(runtime, field_ptr));
        }
    }
    // The parent `obj` falls out of scope here; its `Drop` releases the
    // original constructor (and the per-field counts the parent held).
    drop(obj);
    // SAFETY: every element of `out` was initialised in the loop above
    // (`0..N` covers the whole array exactly once); transmute is sound
    // because `[MaybeUninit<T>; N]` and `[T; N]` share layout.
    Ok(out.map(|cell| unsafe { cell.assume_init() }))
}

/// Read the tag byte of a constructor object.
///
/// Used by sum-type decoders (`Option` and `except::Except` (sibling-module sum-type carriers))
/// that need to pick a variant before they know the arity. Borrow-only:
/// leaves the refcount untouched.
///
/// # Errors
///
/// Returns [`HostStage::Conversion`](crate::error::HostStage::Conversion)
/// if `obj` is not a heap-allocated constructor.
pub fn ctor_tag(obj: &Obj<'_>) -> LeanResult<u8> {
    view(obj).ctor().map(|ctor| ctor.tag())
}

/// Shared validator for [`take_ctor_objects`]: ctor kind, matching tag,
/// matching `num_objs`.
fn require_ctor_shape(obj: &Obj<'_>, expected_tag: u8, expected_num_objs: usize, label: &str) -> LeanResult<()> {
    let _ = view(obj).ctor_shape(expected_tag, expected_num_objs, label)?;
    Ok(())
}

#[inline]
fn ctor_scalar_tail_size(ptr: *mut lean_rs_sys::lean_object) -> usize {
    // SAFETY: callers have validated `ptr` is a constructor. The scalar
    // tail starts immediately after the object-pointer slots. Lean's
    // object-data-byte-size helper reports the initialized value
    // representation span for the object shape produced by
    // `lean_alloc_ctor`, so the difference is the readable scalar-tail
    // length.
    unsafe {
        let scalar_start = lean_ctor_scalar_cptr(ptr) as usize;
        let object_start = ptr as usize;
        let scalar_offset = scalar_start.saturating_sub(object_start);
        lean_object_data_byte_size(ptr).saturating_sub(scalar_offset)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use core::ffi::c_char;

    use lean_rs_sys::ctor::{lean_alloc_ctor, lean_ctor_set_uint8, lean_ctor_set_uint64};
    use lean_rs_sys::object::{lean_box, lean_is_exclusive, lean_is_shared};
    use lean_rs_sys::string::lean_mk_string;

    use super::{alloc_ctor_with_objects, take_ctor_objects, view};
    use crate::runtime::LeanRuntime;
    use crate::runtime::obj::Obj;

    fn runtime() -> &'static LeanRuntime {
        LeanRuntime::init().expect("runtime init must succeed")
    }

    fn scalar_obj(runtime: &LeanRuntime, payload: usize) -> Obj<'_> {
        // SAFETY: `lean_box` is pointer-bit construction; scalar-tagged
        // objects are valid `Obj` handles and refcount operations are no-ops.
        unsafe { Obj::from_owned_raw(runtime, lean_box(payload)) }
    }

    fn heap_string(runtime: &LeanRuntime) -> Obj<'_> {
        let cstr = c"field".as_ptr().cast::<c_char>();
        // SAFETY: `cstr` is a static NUL-terminated UTF-8 string, and
        // `lean_mk_string` returns an owned Lean object.
        unsafe { Obj::from_owned_raw(runtime, lean_mk_string(cstr)) }
    }

    fn ctor_with_scalar_tail(runtime: &LeanRuntime) -> Obj<'_> {
        // SAFETY: allocate a ctor with no object fields and 16 scalar
        // bytes, then initialize the bytes read by the test before the
        // object escapes.
        unsafe {
            let raw = lean_alloc_ctor(2, 0, 16);
            lean_ctor_set_uint8(raw, 0, 1);
            lean_ctor_set_uint64(raw, 8, 0x0102_0304_0506_0708);
            Obj::from_owned_raw(runtime, raw)
        }
    }

    #[test]
    fn view_discriminates_scalar_and_constructor() {
        let runtime = runtime();
        let scalar = scalar_obj(runtime, 3);
        assert!(view(&scalar).is_scalar());
        assert_eq!(view(&scalar).scalar_payload("TestScalar").expect("scalar payload"), 3);
        assert_eq!(view(&scalar).sum_tag().expect("scalar sum tag"), 3);

        let ctor = alloc_ctor_with_objects(runtime, 1, []);
        let ctor_view = view(&ctor).ctor().expect("ctor view");
        assert!(!view(&ctor).is_scalar());
        assert_eq!(ctor_view.tag(), 1);
    }

    #[test]
    fn constructor_shape_checks_tag_and_object_field_count() {
        let runtime = runtime();
        let ctor = alloc_ctor_with_objects(runtime, 4, [scalar_obj(runtime, 9)]);
        let ctor_view = view(&ctor).ctor_shape(4, 1, "OneField").expect("expected ctor shape");
        assert_eq!(ctor_view.tag(), 4);

        assert!(view(&ctor).ctor_shape(5, 1, "OneField").is_err());
        assert!(view(&ctor).ctor_shape(4, 2, "OneField").is_err());
    }

    #[test]
    fn scalar_tail_reads_are_bounds_checked() {
        let runtime = runtime();
        let ctor = ctor_with_scalar_tail(runtime);
        let ctor_view = view(&ctor).ctor_shape(2, 0, "ScalarTail").expect("expected ctor shape");

        assert!(ctor_view.bool(0, "ScalarTail.flag").expect("bool tail"));
        assert_eq!(
            ctor_view.uint64(8, "ScalarTail.count").expect("u64 tail"),
            0x0102_0304_0506_0708,
        );
        assert!(ctor_view.uint64(9, "ScalarTail.count").is_err());
        assert!(ctor_view.uint8(16, "ScalarTail.flag").is_err());
    }

    #[test]
    fn malformed_object_shape_errors_without_panicking() {
        let runtime = runtime();
        let scalar = scalar_obj(runtime, 0);
        assert!(view(&scalar).ctor().is_err());

        let wide_scalar = scalar_obj(runtime, usize::from(u8::MAX) + 1);
        assert!(view(&wide_scalar).sum_tag().is_err());

        let ctor = ctor_with_scalar_tail(runtime);
        assert!(view(&ctor).scalar_payload("ExpectedScalar").is_err());
    }

    #[test]
    fn take_ctor_objects_preserves_field_ownership() {
        let runtime = runtime();
        let child = heap_string(runtime);
        let witness = child.clone();

        let parent = alloc_ctor_with_objects(runtime, 0, [child]);
        let [taken] = take_ctor_objects::<1>(parent, 0, "Parent").expect("take field");

        // SAFETY: header-only refcount observations of live owned objects.
        assert!(unsafe { lean_is_shared(taken.as_raw_borrowed()) });
        assert!(unsafe { lean_is_shared(witness.as_raw_borrowed()) });

        drop(taken);
        // SAFETY: after dropping the extracted field, only `witness`
        // remains. If `take_ctor_objects` failed to balance the parent
        // and child refcounts, this would stay shared or ASan would catch
        // the double-release path.
        assert!(unsafe { lean_is_exclusive(witness.as_raw_borrowed()) });
        drop(witness);
    }
}
