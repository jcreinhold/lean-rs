#![no_main]
//! `cargo +nightly fuzz run abi_decode`—drives `lean_rs::abi`'s
//! decoders with structured Lean-shaped inputs and asserts the
//! contract: every decode returns either `Ok(_)` or
//! `Err(LeanError::Host(stage = Conversion))`. Any panic, any other
//! error kind, or any sanitizer-detected fault is a fuzzing finding.
//!
//! Inputs are constructed via the `lean-rs-sys` public allocators
//! (`lean_box`, `lean_mk_string`, `lean_alloc_sarray`,
//! `lean_alloc_array`, `lean_alloc_ctor`, `lean_box_uint64`). The
//! generators bound every allocation size so a single fuzz iteration
//! cannot wedge libfuzzer; `lean-rs-sys`'s ownership rules are honoured
//! by transferring exactly one refcount per allocation into the
//! `lean_rs::fuzz_entry::decode_*` wrappers, which themselves wrap the
//! pointer in `Obj<'lean>` and let `Drop` release the count on the
//! happy and unhappy paths alike.

use arbitrary::{Arbitrary, Unstructured};
use lean_rs::LeanRuntime;
use lean_rs::fuzz_entry::{
    LeanError, decode_array_u64, decode_bytearray, decode_ctor_tag, decode_except, decode_nat_u64,
    decode_option_u64, decode_string,
};
use lean_rs_sys::array::{lean_alloc_array, lean_alloc_sarray, lean_array_cptr, lean_sarray_cptr};
use lean_rs_sys::ctor::{lean_alloc_ctor, lean_box_uint64, lean_ctor_obj_cptr};
use lean_rs_sys::lean_object;
use lean_rs_sys::object::lean_box;
use lean_rs_sys::string::lean_mk_string;
use libfuzzer_sys::fuzz_target;

const MAX_ARRAY_LEN: usize = 64;
const MAX_STRING_LEN: usize = 256;
const MAX_CTOR_FIELDS: usize = 8;

/// The decoder family the fuzzer dispatches to. Each variant carries a
/// generator-shape payload bounded to keep allocations small.
#[derive(Arbitrary, Debug)]
enum DecoderInput {
    String { len: u16, bytes: Vec<u8> },
    ByteArray { len: u16, bytes: Vec<u8> },
    ArrayU64 { len: u8, scrambled_tag: bool, values: Vec<u64> },
    OptionU64 { tag: u8, payload: u64 },
    Except { tag: u8, payload: ExceptPayload },
    NatU64 { kind: NatShape },
    CtorTag { tag: u8, num_objs: u8, scalar_sz: u16 },
    /// Pass a raw scalar through; many decoders should reject it.
    Scalar { raw: u8 },
}

#[derive(Arbitrary, Debug)]
enum ExceptPayload {
    OkInt(u64),
    OkString(Vec<u8>),
    ErrInt(u64),
    ErrString(Vec<u8>),
}

#[derive(Arbitrary, Debug)]
enum NatShape {
    Scalar(u32),
    SmallBoxed(u32),
    /// Pass a constructor instead of a scalar so the decoder reports a
    /// typed `Conversion` error.
    WrongTag(u8),
}

fuzz_target!(|data: &[u8]| {
    let mut u = Unstructured::new(data);
    let input = match DecoderInput::arbitrary(&mut u) {
        Ok(v) => v,
        Err(_) => return,
    };
    let runtime = match LeanRuntime::init() {
        Ok(r) => r,
        Err(_) => return,
    };

    // Each branch constructs a Lean object owning one refcount and
    // hands it to a decoder. The decoder either returns `Ok` (an
    // explicit success) or `Err(LeanError::Host)` (a typed
    // `Conversion` failure). Any other outcome is a fuzz finding.
    match input {
        DecoderInput::String { len, bytes } => {
            let cstr = make_cstring(bytes, len as usize);
            // SAFETY: `lean_mk_string` returns one owned refcount; we
            // transfer it into `decode_string`'s `Obj` wrap.
            let raw = unsafe { lean_mk_string(cstr.as_ptr()) };
            // SAFETY: `raw` is freshly allocated by lean_mk_string.
            let result = unsafe { decode_string(runtime, raw) };
            assert_clean(result.map(|_| ()));
        }
        DecoderInput::ByteArray { len, bytes } => {
            let bounded = bounded_len(len as usize, MAX_STRING_LEN);
            // SAFETY: `lean_alloc_sarray(1, n, n)` allocates a fresh
            // packed-byte array; we then write the user-supplied bytes
            // before transferring ownership.
            let raw = unsafe { lean_alloc_sarray(1, bounded, bounded) };
            // SAFETY: `raw` is a fresh sarray; cptr is valid for `bounded` bytes.
            unsafe {
                let dst = lean_sarray_cptr(raw);
                for i in 0..bounded {
                    let byte = bytes.get(i).copied().unwrap_or(0);
                    *dst.add(i) = byte;
                }
            }
            // SAFETY: `raw` owns one refcount; transferred to the decoder.
            let result = unsafe { decode_bytearray(runtime, raw) };
            assert_clean(result.map(|_| ()));
        }
        DecoderInput::ArrayU64 {
            len,
            scrambled_tag,
            values,
        } => {
            let bounded = bounded_len(len as usize, MAX_ARRAY_LEN);
            // SAFETY: `lean_alloc_array(n, n)` allocates a fresh array
            // with `n` reserved object slots.
            let raw = unsafe { lean_alloc_array(bounded, bounded) };
            // SAFETY: write `bounded` polymorphic-boxed `UInt64` slots.
            unsafe {
                let slots = lean_array_cptr(raw);
                for i in 0..bounded {
                    let v = values.get(i).copied().unwrap_or(0);
                    let boxed = lean_box_uint64(v);
                    *slots.add(i) = boxed;
                }
            }
            // Optionally swap one element for a wrong-kind value so the
            // decoder's per-slot validation exercises its error path.
            if scrambled_tag && bounded > 0 {
                // SAFETY: in-bounds write of a scalar tag onto slot 0.
                unsafe {
                    let slots = lean_array_cptr(raw);
                    let bad = lean_box(7);
                    // Release the boxed UInt64 we are about to overwrite.
                    lean_rs_sys::refcount::lean_dec(*slots);
                    *slots = bad;
                }
            }
            // SAFETY: ownership transferred.
            let result = unsafe { decode_array_u64(runtime, raw) };
            assert_clean(result.map(|_| ()));
        }
        DecoderInput::OptionU64 { tag, payload } => {
            // Lean's Option encoding: `None` = `lean_box(0)`, `Some x` =
            // ctor(tag=1, num_objs=1) carrying a polymorphic-boxed x.
            let raw = match tag % 4 {
                0 => unsafe { lean_box(0) }, // None
                1 => build_some(payload),    // valid Some
                2 => build_wrong_tag_ctor(),
                _ => unsafe { lean_box(payload as usize & 0x7fff) }, // bogus scalar
            };
            // SAFETY: every branch produces an owned refcount.
            let result = unsafe { decode_option_u64(runtime, raw) };
            assert_clean(result.map(|_| ()));
        }
        DecoderInput::Except { tag, payload } => {
            let raw = build_except(tag, &payload);
            // SAFETY: `build_except` returns one owned refcount.
            let result = unsafe { decode_except(runtime, raw) };
            assert_clean(result.map(|_| ()));
        }
        DecoderInput::NatU64 { kind } => {
            let raw = match kind {
                NatShape::Scalar(n) => unsafe { lean_box(n as usize) },
                NatShape::SmallBoxed(n) => unsafe { lean_box(n as usize & 0x7fff) },
                NatShape::WrongTag(t) => unsafe { lean_alloc_ctor(t.min(15), 0, 0) },
            };
            // SAFETY: each branch owns one refcount.
            let result = unsafe { decode_nat_u64(runtime, raw) };
            assert_clean(result.map(|_| ()));
        }
        DecoderInput::CtorTag {
            tag,
            num_objs,
            scalar_sz,
        } => {
            // Bound the ctor parameters so `lean_alloc_ctor`'s
            // preconditions (`tag <= LEAN_MAX_CTOR_TAG`, `num_objs <=
            // u8::MAX`) are satisfied. `scalar_sz` is bounded to keep
            // allocations small.
            let bounded_tag = tag.min(240);
            let bounded_num = num_objs.min(MAX_CTOR_FIELDS as u8);
            let bounded_scalar = (scalar_sz as usize).min(64);
            // SAFETY: bounded parameters above are within `lean_alloc_ctor`'s contract.
            let raw = unsafe { lean_alloc_ctor(bounded_tag, bounded_num, bounded_scalar) };
            // Fill object slots with `lean_box(0)` so the ctor body is
            // well-formed; otherwise reading any field via the decoders
            // we may chain in would read uninit memory.
            if bounded_num > 0 {
                // SAFETY: ctor was just allocated with `bounded_num` slots.
                unsafe {
                    let slots = lean_ctor_obj_cptr(raw);
                    for i in 0..(bounded_num as usize) {
                        *slots.add(i) = lean_box(0);
                    }
                }
            }
            // SAFETY: ownership transferred.
            let result = unsafe { decode_ctor_tag(runtime, raw) };
            assert_clean(result.map(|_| ()));
        }
        DecoderInput::Scalar { raw: payload } => {
            // Feed a raw scalar pointer to multiple decoders—they
            // should all reject it cleanly.
            let scalar = unsafe { lean_box(payload as usize) };
            // Bump refcount five times by re-boxing the same scalar so
            // each decoder receives its own ownership (scalars share an
            // immortal pointer; `lean_inc` is a no-op on them, so the
            // `Drop` chain is also a no-op).
            // SAFETY: every scalar pointer is its own owned reference.
            assert_clean(unsafe { decode_string(runtime, scalar) }.map(|_| ()));
            assert_clean(unsafe { decode_bytearray(runtime, scalar) }.map(|_| ()));
            assert_clean(unsafe { decode_option_u64(runtime, scalar) }.map(|_| ()));
            assert_clean(unsafe { decode_array_u64(runtime, scalar) }.map(|_| ()));
            assert_clean(unsafe { decode_nat_u64(runtime, scalar) }.map(|_| ()));
        }
    }
});

fn make_cstring(mut bytes: Vec<u8>, bound: usize) -> std::ffi::CString {
    let cap = bound.min(MAX_STRING_LEN);
    bytes.truncate(cap);
    // Replace embedded NULs so `CString::new` does not error.
    for b in bytes.iter_mut() {
        if *b == 0 {
            *b = b' ';
        }
    }
    std::ffi::CString::new(bytes).expect("NUL bytes were rewritten above")
}

fn bounded_len(requested: usize, ceiling: usize) -> usize {
    requested.min(ceiling)
}

fn build_some(payload: u64) -> *mut lean_object {
    // SAFETY: `lean_alloc_ctor(1, 1, 0)` allocates a single-object ctor;
    // we then write a polymorphic-boxed payload into slot 0.
    unsafe {
        let raw = lean_alloc_ctor(1, 1, 0);
        let slots = lean_ctor_obj_cptr(raw);
        *slots = lean_box_uint64(payload);
        raw
    }
}

fn build_wrong_tag_ctor() -> *mut lean_object {
    // A ctor with tag 7 is neither `None` nor `Some`; decoders must
    // surface a typed `Conversion` error.
    // SAFETY: `lean_alloc_ctor(7, 0, 0)` is within the helper's contract.
    unsafe { lean_alloc_ctor(7, 0, 0) }
}

fn build_except(tag: u8, payload: &ExceptPayload) -> *mut lean_object {
    // Lean's `Except E T` ctors: `error` = tag 0, `ok` = tag 1, both
    // carry one object field. A ctor with a tag outside {0, 1} forces
    // a typed `Conversion` error.
    let ctor_tag = tag % 3;
    // SAFETY: bounded ctor parameters.
    unsafe {
        let raw = lean_alloc_ctor(ctor_tag, 1, 0);
        let slots = lean_ctor_obj_cptr(raw);
        *slots = build_except_payload(payload);
        raw
    }
}

fn build_except_payload(payload: &ExceptPayload) -> *mut lean_object {
    match payload {
        ExceptPayload::OkInt(n) | ExceptPayload::ErrInt(n) => unsafe { lean_box_uint64(*n) },
        ExceptPayload::OkString(bytes) | ExceptPayload::ErrString(bytes) => {
            let cstr = make_cstring(bytes.clone(), MAX_STRING_LEN);
            // SAFETY: `cstr` is NUL-terminated UTF-8 (post-rewrite).
            unsafe { lean_mk_string(cstr.as_ptr()) }
        }
    }
}

fn assert_clean(result: Result<(), LeanError>) {
    match result {
        Ok(()) => {}
        Err(LeanError::Host(_)) => {}
        Err(LeanError::LeanException(exc)) => {
            panic!("unexpected LeanException from a pure-decode path: {:?}", exc.kind());
        }
        // Catch-all for future variants—fuzzing must surface any
        // variant the decoder gains that is not yet whitelisted.
        Err(other) => panic!("unexpected LeanError variant: {other:?}"),
    }
}
