# Unsafe Inventory

Every `unsafe` block and `pub unsafe fn` declaration in the workspace, paired with the invariant
that makes it sound. The companion document `docs/architecture/01-safety-model.md` states the
thesis these invariants serve; this file is the audit that proves the thesis holds today and the
checklist that future prompts read before changing any unsafe seam.

Layering: raw `lean_*` symbols enter the workspace **only** through `lean-rs-sys`. `lean-rs`
consumes them through per-file `#![allow(unsafe_code)]` opt-outs (or per-block
`#[allow(unsafe_code)]` attributes); `lean-toolchain` has no `unsafe` at all. The
`lean-rs-sys` section below is therefore the load-bearing boundary, and the per-file `lean-rs`
sections cite the `lean-rs-sys` symbol(s) each block invokes.

When a file is changed in a way that adds, removes, or re-shapes an `unsafe` block, the
inventory entry must be updated in the same commit (recorded as a maintenance rule in
`CLAUDE.md`).

## Crate: `lean-rs-sys` — the load-bearing boundary

`lean-rs-sys` is the single crate-wide `#[allow(unsafe_code)]` opt-out in the workspace
(`crates/lean-rs-sys/src/lib.rs:32`). Per `RD-2026-05-17-005`, public types are opaque
(`lean_object` is `[u8; 0] + PhantomData<(*mut u8, PhantomPinned)>`) and downstream code reaches
state only through `pub unsafe fn` helpers. Each helper documents its caller obligations in a
`# Safety` doc; each `unsafe { ... }` block inside the body restates the specific
ABI/layout invariant it relies on.

The crate's blanket allow is intentional: every helper is `unsafe` because Lean's ABI
ownership/borrow rules cannot be enforced by Rust types alone. The boundary is justified by the
build-time `EXPECTED_HEADER_DIGEST` check (header bytes pinned) and the `REQUIRED_SYMBOLS`
link-time test (every symbol the inline helpers cast through is exported by `libleanshared`).

Entries below group blocks by `pub unsafe fn` — the function-level `# Safety` doc is the
authoritative statement of the invariant, and the in-body blocks rely on the same caller
obligations.

### `crates/lean-rs-sys/src/object.rs` — 13 `pub unsafe fn`, 12 blocks

Object inspection, tagging, and runtime-mode reads. Mirrors `lean.h:312–630`.

| `pub unsafe fn` (line) | `lean-rs-sys` symbol(s) | Invariant |
| --- | --- | --- |
| `lean_is_scalar` (51) | pointer-bit math, no deref | `o` is any Lean object pointer; scalar encoding is pure pointer arithmetic. |
| `lean_box` (63) | pointer-bit math | `n` fits in the scalar payload width; result is a scalar-tagged non-null pointer. |
| `lean_unbox` (74) | pointer-bit math | `o` is a scalar-tagged pointer (`lean_is_scalar(o)` would return `true`). |
| `header` (82) | layout cast `*o.cast::<LeanObjectRepr>()` | `o` is a non-scalar live Lean object; layout pinned by `EXPECTED_HEADER_DIGEST`. |
| `lean_alloc_object` extern wrapper (91) | `lean_alloc_object` | size is non-zero; result is owned or null on OOM (Lean aborts internally). |
| `lean_ptr_tag` (103) | `header(o).m_tag` | `o` is a live non-scalar object. |
| `lean_ptr_other` (114) | `header(o).m_other` | `o` is a live non-scalar object whose `m_other` byte is defined for its tag. |
| `lean_is_*` predicates (macro-stamped) | `lean_ptr_tag(o) == TAG` | as above. |
| `lean_is_ctor` (141) | `lean_ptr_tag(o) <= LEAN_MAX_CTOR_TAG` | as above. |
| `lean_obj_tag` (197) | `header(o)` read of constructor tag | `o` is a live non-scalar constructor; `m_tag` encodes the ctor index up to a saturating `u32`. |
| `lean_is_st`, `lean_is_mt`, `lean_is_persistent`, `lean_is_exclusive`, `lean_is_shared` (214–259) | `load_rc(o)` | `o` is a live non-scalar object; refcount sign rules per `refcount.rs` module doc. |

### `crates/lean-rs-sys/src/refcount.rs` — 6 `pub unsafe fn`, 8 blocks

Refcount fast paths; the actual atomic operations happen through
`AtomicI32::from_ptr(&raw mut (*repr).m_rc)` (Rust 1.75+), so the load/store/`fetch_sub` site
sees a safe `&AtomicI32`.

| `pub unsafe fn` (line) | `lean-rs-sys` symbol(s) | Invariant |
| --- | --- | --- |
| `rc_atom` (private, ~38) | `AtomicI32::from_ptr` over `(*LeanObjectRepr).m_rc` | `o` is a live non-scalar Lean object; layout pinned. |
| `lean_inc_ref_n` (68) | `lean_inc_ref_n_cold` (MT slow path) | `o` is a live non-scalar object; `n >= 1`. |
| `lean_inc_ref` (91) | delegates to `lean_inc_ref_n` | as above with `n = 1`. |
| `lean_inc` (105) | scalar short-circuit + `lean_inc_ref` | `o` is a live object pointer (scalar or non-scalar). |
| `lean_inc_n` (122) | scalar short-circuit + `lean_inc_ref_n` | `o` is a live object pointer; `n >= 1`. |
| `lean_dec_ref` (148) | `lean_dec_ref_cold` (MT/free path) | `o` is a live non-scalar object whose RC the caller transferred. |
| `lean_dec` (172) | scalar short-circuit + `lean_dec_ref` | `o` is a live object pointer whose RC the caller transferred. |

### `crates/lean-rs-sys/src/ctor.rs` — 24 `pub unsafe fn`, 36 blocks

Constructor objects, polymorphic boxing, and per-width scalar getters/setters. Mirrors
`lean.h:642–760` + `lean.h:2811–2873`.

| `pub unsafe fn` (line) | `lean-rs-sys` symbol(s) | Invariant |
| --- | --- | --- |
| `lean_alloc_ctor` (113) | `lean_alloc_object` + write `LeanCtorObjectRepr.m_tag`, `m_other`, `m_num_objs`, `m_scalar_size` | `tag <= LEAN_MAX_CTOR_TAG`; `num_objs <= 256`; `scalar_sz <= u16::MAX`; result owns one refcount. |
| `lean_ctor_num_objs` (59) | `lean_ptr_other(o)` | `o` is a live ctor object. |
| `lean_ctor_obj_cptr` (72) | layout cast to `LeanCtorObjectRepr.objs` array | `o` is a live ctor with `num_objs` slots. |
| `lean_ctor_scalar_cptr` (86) | layout cast past the object pointer array | `o` is a live ctor whose scalar payload follows `num_objs * sizeof(ptr)`. |
| `lean_box_uint32` / `_uint64` / `_usize` / `_float` (140–229) | `lean_alloc_ctor(0, 0, sizeof(v))` + scalar write | none beyond `lean_alloc_ctor`'s; `v` fits the boxed width. |
| `lean_unbox_uint32` / `_uint64` / `_usize` / `_float` (160–246) | `lean_ctor_get_*(o, 0)` | `o` is a polymorphic-boxed scalar produced by the matching `lean_box_*`. |
| `lean_ctor_get_usize` (259) | `lean_ctor_obj_cptr(o).add(i).cast::<usize>().read_unaligned()` | `o` is a ctor and `i` indexes a slot wide enough for `usize`. |
| `lean_ctor_get_uint8`/`_16`/`_32`/`_64` (276–) | `lean_ctor_scalar_cptr(o).add(offset).cast::<…>().read_unaligned()` | `o` is a ctor and `offset` is in-range for the scalar payload, aligned by the ctor's tag. |
| `lean_ctor_set_*` family | symmetric writes to the scalar/object slots | as above; `i` / `offset` in-range; writing an object slot transfers one refcount. |

### `crates/lean-rs-sys/src/array.rs` — 11 `pub unsafe fn`, 13 blocks

Object arrays (`Array α`) and scalar arrays (`ByteArray`, `FloatArray`, …). Mirrors `lean.h:815–1028`.

| `pub unsafe fn` (line) | `lean-rs-sys` symbol(s) | Invariant |
| --- | --- | --- |
| `lean_alloc_array` (44) | `lean_alloc_object` + write `LeanArrayObjectRepr.{size, capacity}` | `capacity >= size`; both fit `usize`; result owns one refcount with `size` initialised pointer slots logically reserved. |
| `lean_alloc_sarray` (82) | `lean_alloc_object` + write `LeanSArrayObjectRepr.{elem_size, size, capacity}` | `elem_size > 0`; `capacity >= size`; payload bytes are uninit until written. |
| `as_array` (105, private) | layout cast `*o.cast::<LeanArrayObjectRepr>()` | `o` is a live array object. |
| `as_sarray` (111, private) | layout cast `*o.cast::<LeanSArrayObjectRepr>()` | `o` is a live scalar-array object. |
| `lean_array_size` (122) | `as_array(o).size` | `o` is a live array. |
| `lean_array_capacity` (133) | `as_array(o).capacity` | as above. |
| `lean_array_cptr` (145) | pointer arithmetic past header to `data: [*mut lean_object; size]` | `o` is a live array; returned pointer is valid for `size` reads/writes. |
| `lean_array_get_core` (157) | `*lean_array_cptr(o).add(i)` | `i < lean_array_size(o)`; returned pointer is a borrow (no RC transfer). |
| `lean_array_set_core` (169) | `*lean_array_cptr(o).add(i) = v` | `i < lean_array_capacity(o)`; `v` owns one refcount transferred into the slot. |
| `lean_sarray_elem_size`, `lean_sarray_size`, `lean_sarray_capacity`, `lean_sarray_cptr` (180–214) | symmetric reads against `LeanSArrayObjectRepr` | `o` is a live scalar-array; cptr is valid for `size * elem_size` bytes. |

### `crates/lean-rs-sys/src/closure.rs` — 7 `pub unsafe fn`, 8 blocks

Closure objects. Mirrors `lean.h:762–813`.

| `pub unsafe fn` (line) | `lean-rs-sys` symbol(s) | Invariant |
| --- | --- | --- |
| `lean_alloc_closure` (290) | `lean_alloc_object` + write `LeanClosureObjectRepr.{fun, arity, num_fixed}` | `fun` is a valid function pointer expecting `arity` Lean args; `num_fixed <= arity`; payload slots uninit until filled. |
| `as_closure` (~200, private) | layout cast | `o` is a live closure. |
| `lean_closure_fun`, `lean_closure_arity`, `lean_closure_num_fixed` (212–234) | header reads | `o` is a live closure. |
| `lean_closure_arg_cptr` (246) | pointer past header to fixed-arg slot array | `o` is a live closure; returned pointer is valid for `num_fixed` reads/writes. |
| `lean_closure_get` (257), `lean_closure_set` (269) | indexed read/write | `i < num_fixed`; `set` transfers one refcount. |

### `crates/lean-rs-sys/src/string.rs` — 5 `pub unsafe fn`, 6 blocks

String objects. Mirrors `lean.h:1157–1234`.

| `pub unsafe fn` (line) | `lean-rs-sys` symbol(s) | Invariant |
| --- | --- | --- |
| `as_string` (~38, private) | layout cast `*o.cast::<LeanStringObjectRepr>()` | `o` is a live string object (`lean_is_string(o)` would return `true`). |
| `lean_string_size`, `lean_string_len`, `lean_string_capacity` (48–70) | header reads | `o` is a live string. |
| `lean_string_cstr` (82) | pointer past header to the NUL-terminated UTF-8 payload | as above; returned pointer is valid for `lean_string_size(o)` bytes including NUL. |
| `lean_string_byte_size` (100) | `size_of::<LeanStringObjectRepr>() + lean_string_capacity(o)` (saturating) | as above. |

### `crates/lean-rs-sys/src/scalar.rs` — 12 `pub unsafe fn`, 12 blocks

Boxed-scalar conversions. Mirrors `lean.h:1356–2065`.

| `pub unsafe fn` (line) | `lean-rs-sys` symbol(s) | Invariant |
| --- | --- | --- |
| `lean_usize_to_nat` (29) | `lean_box(n)` if `n <= LEAN_MAX_SMALL_NAT`; else extern `lean_cstr_to_nat_*` | always sound; result owns one refcount. |
| `lean_unsigned_to_nat` (47) | delegates to `lean_usize_to_nat` | none. |
| `lean_uint64_to_nat` (59) | scalar fast path or `lean_cstr_to_nat` for the 64-bit overflow region | none. |
| `lean_uint8_of_nat` (77) | `lean_obj_tag` + scalar / bignum dispatch | `a` is a `Nat` (scalar or bignum); result truncates to `u8`. |
| `lean_uint8_to_nat`, `lean_uint16_to_nat`, `lean_uint32_to_nat` (94–116) | widening to `usize` then `lean_usize_to_nat` | none. |
| `lean_int_to_int` (128), `lean_int64_to_int` (150) | scalar fast path or extern `lean_cstr_to_int` | `n` fits the requested representation; result owns one refcount. |
| `lean_nat_to_int` (168) | `Nat → Int` coercion via extern | `a` is an owned `Nat`; result owns one refcount. |
| `lean_scalar_to_int64` (189), `lean_scalar_to_int` (206) | unbox + sign-extend | `a` is a scalar-tagged `Int` (`lean_is_scalar(a)` true). |

### `crates/lean-rs-sys/src/io.rs` — 5 `pub unsafe fn`, 6 blocks

`IO` result helpers. Mirrors `lean.h:2893–2907`.

| `pub unsafe fn` (line) | `lean-rs-sys` symbol(s) | Invariant |
| --- | --- | --- |
| `ctor_get0` (~20, private) | `lean_ctor_obj_cptr(r).read()` | `r` is a live ctor with at least one object slot. |
| `lean_io_result_is_ok` (35), `lean_io_result_is_error` (46) | `lean_ptr_tag(r) == {0,1}` | `r` is a live non-scalar `IO α` result (tag 0 = `ok`, tag 1 = `error`). |
| `lean_io_result_get_value` (59), `lean_io_result_get_error` (70) | borrowed `ctor_get0(r)` | as above; returned pointer is a borrow (no RC transfer). |
| `lean_io_result_take_value` (83) | move out then `lean_dec(r)` | `r` is an owned `IO α` result; caller owns the returned value's refcount. |

### `crates/lean-rs-sys/src/external.rs` — 3 `pub unsafe fn`, 3 blocks

External objects (C `void*` payloads). Mirrors `lean.h:295–1332`.

| `pub unsafe fn` (line) | `lean-rs-sys` symbol(s) | Invariant |
| --- | --- | --- |
| `lean_alloc_external` (35) | `lean_alloc_object` + write `LeanExternalObjectRepr.{class, data}` | `cls` is a valid `lean_external_class` pointer for `data`. |
| `lean_get_external_class` (58), `lean_get_external_data` (70) | header reads | `o` is a live external object. |

### `crates/lean-rs-sys/src/init.rs` — 0 blocks (7 extern declarations)

`lean_initialize`, `lean_initialize_runtime_module`, `lean_initialize_thread`,
`lean_finalize_thread`, `lean_setup_args`, `lean_init_task_manager`,
`lean_init_task_manager_using`, `lean_finalize_task_manager`. Externs only — the body lives in
`libleanshared`. Calling any of them is `unsafe` per Lean's runtime entry-point rules:
`lean_initialize` exactly once per process; `lean_initialize_thread` paired with
`lean_finalize_thread` on every worker thread; `lean_setup_args` argv must outlive readers.

### `crates/lean-rs-sys/src/nat_int.rs` — 0 blocks (extern declarations)

Bignum externs from `lean.h:1334–1853` (`lean_cstr_to_nat`, `lean_cstr_to_int`, bignum
arithmetic helpers). Externs only.

### `crates/lean-rs-sys/src/repr.rs` — 1 block (in a `#[cfg(test)]` layout assertion)

Crate-private layout structs mirroring `lean.h:131–310`. The one `unsafe` block lives in a test
that asserts the in-memory layout matches the `EXPECTED_HEADER_DIGEST`-pinned bytes; the
production paths never touch `repr` directly except through layout casts in the inline
accessors of the sibling modules.

### `crates/lean-rs-sys/src/lib.rs` — 1 block (`pub unsafe fn` re-export bridge), 2 `pub unsafe fn`

Crate-level `#![allow(unsafe_code)]`. The single block forwards a discovery helper that
returns a `&'static` view of the `REQUIRED_SYMBOLS` table; the `unsafe` is the caller's
invariant that the link-time test has succeeded and the symbols are present.

### `crates/lean-rs-sys/src/consts.rs`, `crates/lean-rs-sys/src/types.rs` — 0 blocks

`consts.rs` is the `build.rs`-resolved version + digest constants. `types.rs` defines opaque
`lean_object` and the calling-convention typedefs; the one `pub unsafe fn` declaration there
is a `Default`-like constructor stub with no body that callers cannot reach.

---

## Crate: `lean-rs` — per-file opt-outs

Every block below ultimately calls a `pub unsafe fn` from `lean-rs-sys` (column "sys symbol(s)
called"); the invariant is whatever that `# Safety` doc requires, satisfied by the call site's
local context. The blanket per-file `#![allow(unsafe_code)]` keeps the opt-out scope as small
as the safety model demands.

### Runtime — `crates/lean-rs/src/runtime/`

#### `runtime/obj.rs` — 19 blocks, `#![allow(unsafe_code)]` at line 20

Owned (`Obj<'lean>`) and borrowed (`ObjRef<'lean, 'a>`) handles. Six production blocks plus the
test module's RC-observation calls.

| Line | Block | sys symbol(s) | Invariant |
| --- | --- | --- | --- |
| 91 | `Obj::from_owned_raw` non-null wrap | `NonNull::new_unchecked` | caller documented non-null + one-refcount transfer in the `# Safety` block above. |
| 148 | `Obj::runtime` ZST borrow synthesis | `NonNull::dangling().as_ref()` | `LeanRuntime` is zero-sized; `'lean` lifetime witnesses initialisation. |
| 167 | `Clone for Obj::clone` | `lean_rs_sys::refcount::lean_inc` | `self.ptr` is a live owned Lean object (refcount >= 1). |
| 182 | `Drop for Obj::drop` | `lean_rs_sys::refcount::lean_dec` | `self.ptr` owns exactly one refcount that `Obj` is about to release. |
| 229 | test helper `scalar_obj` | `lean_box` | `7` fits scalar payload. |
| 239 | test helper `heap_string` | `lean_mk_string` | `c"abc"` is a valid NUL-terminated UTF-8 cstring. |
| 258, 262, 263, 267 | `clone_increments_heap_refcount` predicates | `lean_is_exclusive`, `lean_is_shared` | header-only inspection of live owned object. |
| 277, 283, 288, 290 | `into_raw_does_not_decrement` body | `lean_is_shared`, `lean_dec` | header-only predicates; `lean_dec` releases the count produced by `into_raw`. |
| 298, 305, 311 | `borrow_does_not_adjust_refcount` predicates | `lean_is_exclusive` | header-only inspection. |
| 378 | positive-lifetime sentinel `_lifetime_anchored_to_runtime_borrow` | `lean_box` | scalar pointer arithmetic. |

#### `runtime/init.rs` — 3 blocks, `#![allow(unsafe_code)]` at line 14

`LeanRuntime::init` calls `lean_initialize_runtime_module`, `lean_initialize`,
`lean_init_task_manager`. The triple block at line 106 carries one `// SAFETY:` comment
covering all three: process-once initialisation, sequenced as Lake's compiler expects.

Line 128's `unsafe { NonNull::<LeanRuntime>::dangling().as_ref() }` synthesises the ZST
`&LeanRuntime` from the runtime cell pointer; sound because `LeanRuntime` is zero-sized and the
caller has just verified the cell is initialised.

#### `runtime/thread.rs` — 3 blocks, `#![allow(unsafe_code)]` at line 13

`LeanThreadGuard::attach` calls `lean_initialize_thread`; `Drop` calls `lean_finalize_thread`.
The blocks at lines 67 and 84 carry per-block `// SAFETY:` comments: each pairs an
`attach` / `finalize` on the **same** OS thread, guarded by an RAII handle that cannot be
`Send` (the FFI calls require thread-local Lean state).

### ABI — `crates/lean-rs/src/abi/`

Every block in this directory either (a) wraps a freshly-allocated Lean value as a fresh
`Obj<'lean>` via `Obj::from_owned_raw` (the sys symbol is the matching `lean_alloc_*` or
`lean_*_to_nat` etc.), or (b) inspects a borrowed Lean object's header via a sys predicate
(`lean_is_scalar`, `lean_is_ctor`, `lean_obj_tag`, `lean_ctor_num_objs`, …).

#### `abi/scalar.rs` — 22 blocks, `#![allow(unsafe_code)]` at line 26

Calls the `lean_box*` / `lean_unbox*` family and the predicates. Per-block `// SAFETY:`
comments record either "pure pointer-bit math" (scalar predicates) or "boxed by the
constructor above; layout pinned".

#### `abi/nat.rs` — 8 blocks, `#![allow(unsafe_code)]` at line 17

Calls `lean_uint64_to_nat`, `lean_usize_to_nat`, `lean_unbox`, `lean_is_scalar`, `lean_obj_tag`.
Each block guards a Lean `Nat` scalar/bignum dispatch; bignum branches return a typed
`Conversion` error rather than reading the MPZ payload (see `00-current-state.md` caveats).

#### `abi/int.rs` — 4 blocks, `#![allow(unsafe_code)]` at line 10

Calls `lean_int64_to_int`, `lean_scalar_to_int64`, `lean_is_scalar`, `lean_obj_tag`.

#### `abi/string.rs` — 11 blocks, `#![allow(unsafe_code)]` at line 20

Calls `lean_mk_string`, `lean_is_string`, `lean_string_cstr`, `lean_string_len`,
`lean_is_scalar`, `lean_obj_tag`. The slice constructions at lines 100 and 174 carry
`// SAFETY:` comments naming the lifetime bound (`'a` tied to the source `ObjRef`).

#### `abi/bytearray.rs` — 9 blocks, `#![allow(unsafe_code)]` at line 22

Calls `lean_alloc_sarray`, `lean_sarray_elem_size`, `lean_sarray_cptr`, `lean_sarray_size`,
`lean_is_scalar`, `lean_is_sarray`, `lean_obj_tag`. The alloc block (line 45) writes the byte
payload with the same `elem_size = 1` precondition that gates the read side.

#### `abi/array.rs` — 9 blocks, `#![allow(unsafe_code)]` at line 28

Calls `lean_alloc_array`, `lean_array_size`, `lean_array_cptr`, `lean_array_set_core`,
`lean_array_get_core`, `lean_is_array`, `lean_is_scalar`, `lean_obj_tag`. The `from_iter_exact`
write loop (line 56) carries the `lean_alloc_array(n, n)` precondition that lets every slot be
written exactly once.

#### `abi/option.rs` — 8 blocks, `#![allow(unsafe_code)]` at line 27

Calls `lean_box(0)` for `None`, `lean_is_scalar`, `lean_unbox`, `lean_is_ctor`, `lean_obj_tag`
for `Some`/`None` discrimination. Encodes Lean's mixed-arity nullary-scalar `Option`.

#### `abi/except.rs` — 2 blocks, per-block `#[allow(unsafe_code)]` (no module-level allow)

Two `Obj::from_owned_raw` wraps in the `LeanAbi::from_c` impls for `Except<E, T>` and
`Result<T, E>`. The invariant is "`c` is a `lean_obj_res` owning one refcount per Lake's
contract" — established by the typed function-pointer cast in
`module::exported::LeanExported::call`.

#### `abi/structure.rs` — 10 blocks, `#![allow(unsafe_code)]` at line 38

Calls `lean_alloc_ctor`, `lean_ctor_obj_cptr`, `lean_ctor_num_objs`, `lean_is_scalar`,
`lean_is_ctor`, `lean_obj_tag`. The `take_ctor_objects::<N>` body (line 128) reads the field
slot through `read()` and pairs the read with `lean_inc` on the borrowed pointer so the
caller's `Obj` receives a fresh refcount.

#### `abi/traits.rs` — 1 block, per-block `#[allow(unsafe_code)]` at line 162

The blanket `LeanAbi for Obj<'lean>` identity impl wraps the raw return pointer back into an
`Obj` via `Obj::from_owned_raw`.

#### `abi/tests.rs` — 2 blocks, per-block `#[allow(unsafe_code)]` at line 582

Borrowed-view pointer-equality assertion (no header deref).

### Module — `crates/lean-rs/src/module/`

#### `module/library.rs` — 6 blocks, `#![allow(unsafe_code)]` at line 32

Calls `libloading::Library::new` (line 112) and `library.get` (lines 186, 212, 233, 253). All
`unsafe` for dlopen-time reasons documented at the call site: the loaded library may run
constructors, and resolved symbols are typed by the caller.

#### `module/initializer.rs` — 4 blocks, `#![allow(unsafe_code)]` at line 34

Calls a Lake-emitted module initializer function pointer (line 198, wrapped in
`catch_unwind`) and then wraps the returned `IO α` result as an `Obj` (line 218). Block 116 is
a `from_utf8_unchecked` on bytes already validated by the symbol-bytes builder.

#### `module/exported.rs` — 7 blocks, `#![allow(unsafe_code)]` at line 59

The typed `LeanExported::call` machinery. Each block at lines 246, 310, 314, 473, 489, 512, 517
either wraps a freshly-returned `lean_object*` as an `Obj`, transmutes between `R::CRepr` and
`*mut lean_object` (line 517 — sound because `R: LeanAbi` constrains `CRepr` to either a
scalar primitive or `*mut lean_object`), or dispatches the function-pointer call through the
per-arity macro.

### Fuzzing entry — `crates/lean-rs/src/fuzz_entry.rs`

Feature-gated by `fuzzing` (off by default; not semver-stable). Seven `pub unsafe fn`
wrappers (`decode_string`, `decode_bytearray`, `decode_array_u64`, `decode_option_u64`,
`decode_except`, `decode_nat_u64`, `decode_ctor_tag`) plus seven matching inner blocks.
Each function takes a `*mut lean_object` owning one transferred refcount and wraps it in an
`Obj<'lean>` via `unsafe { Obj::from_owned_raw(runtime, raw) }`; the invariant is the same as
the `Obj::from_owned_raw` `# Safety` doc and is the caller's responsibility — fuzz harnesses
construct the inputs with `lean-rs-sys`'s public allocators. The module lives at the crate
root rather than under `abi/` so the `pub(crate) abi` boundary stays intact when the feature
is off.

### Error — `crates/lean-rs/src/error/`

#### `error/io.rs` — 16 blocks, `#![allow(unsafe_code)]` at line 28

The `IO α` result decoder. Blocks at lines 64, 69, 73, 80, 125, 136, 153, 158, 163, 165, 169,
175 read the `IO.Error` constructor's fields via `lean_io_result_*`, `lean_obj_tag`,
`lean_ctor_num_objs`, `lean_ctor_obj_cptr`, `lean_is_scalar`, `lean_is_string`,
`lean_string_cstr`/`lean_string_len`. The test-support block at line 247 transmutes a resolved
`dlsym` address into a typed function pointer.

#### `error/panic.rs` — 0 blocks

`catch_callback_panic` is pure safe Rust around `std::panic::catch_unwind`.

### Host — `crates/lean-rs/src/host/`

#### `host/session.rs` — 16 blocks, `#![allow(unsafe_code)]` at line 89

Each `LeanSession` method that dispatches into a Lake-installed function constructs a typed
`LeanExported` via `unsafe { LeanExported::from_function_address(runtime, address) }`. The
address comes from `SessionSymbols`, populated by one `dlsym` per symbol at capability load.
The `# Safety` on `from_function_address` requires that `address` was resolved as the correct
typed symbol; `SessionSymbols::resolve` is the single place that obligation is discharged.

#### `host/handle/{name,level,expr,declaration}.rs` — 1 block each, per-block `#[allow(unsafe_code)]`

Each constructs the public handle's inner `Obj` from a freshly-returned `lean_object*`
produced by a fixture export. The invariant is the same as the `LeanExported::call` return
contract.

#### `host/elaboration/failure.rs` — 2 blocks, `#![allow(unsafe_code)]` at line 14

`lean_ctor_get_uint8` reads the `Severity` byte off the failure ctor.

#### `host/elaboration/diagnostic.rs` — 5 blocks, `#![allow(unsafe_code)]` at line 25

`lean_ctor_get_uint8`, `lean_is_scalar`, `lean_unbox`, `lean_obj_tag` reads on the diagnostic
ctor and its severity tag.

#### `host/evidence/handle.rs` — 1 block, per-block `#[allow(unsafe_code)]`

Wraps the evidence handle's `Obj` from a fixture-returned pointer.

#### `host/evidence/status.rs` — 2 blocks, per-block `#[allow(unsafe_code)]` (lines 103, 108)

Reads the `EvidenceStatus` scalar tag (`lean_is_scalar` + `lean_unbox`), with a heap-ctor
fallback gated by `lean_obj_tag` (not currently triggered but kept for forward-compat with a
Lean representation change).

---

## Crate: `lean-toolchain` — 0 unsafe blocks

`rg -n "unsafe" crates/lean-toolchain/src` returns nothing. The crate is build-time-only
(toolchain discovery, fingerprinting, link diagnostics) and consumes `lean-rs-sys` constants
through their safe re-export. Any new `unsafe` here would require a `#![allow(unsafe_code)]`
opt-out with reviewer sign-off per `docs/architecture/01-safety-model.md`.

---

## Sanitizer & leak-check instructions

### Local — AddressSanitizer on Linux nightly

```sh
rustup toolchain install nightly --component rust-src
cd crates/lean-rs
RUSTFLAGS="-Z sanitizer=address -Cdebug-assertions=on" \
RUSTDOCFLAGS="-Z sanitizer=address" \
LEAN_RS_REFCOUNT_STRESS_ITERS=20000 \
cargo +nightly test --target x86_64-unknown-linux-gnu --tests \
  -- --include-ignored --test-threads=1
```

`-Z sanitizer=address` instruments every allocation. On Linux the LeakSanitizer ships with
ASan by default, so the same run detects both refcount use-after-free and session-loop leaks.
`--include-ignored` activates the long-iteration variants of the stress and session-leak loop
tests; `LEAN_RS_REFCOUNT_STRESS_ITERS` overrides the per-test iteration count for the
runtime-`Obj` stress tests. `--test-threads=1` keeps the per-thread Lean runtime invariant
intact.

### Local — fuzz target (Linux or macOS, nightly)

```sh
rustup toolchain install nightly --component rust-src
cargo install cargo-fuzz                 # one-shot
cd crates/lean-rs/fuzz
cargo +nightly fuzz run abi_decode -- \
  -runs=200000 -max_total_time=120
```

The `abi_decode` target drives `lean_rs::abi`'s `{string, bytearray, array, option, except,
structure}` decoders with `Arbitrary`-generated Lean-shaped inputs constructed via `lean-rs-sys`
public helpers. Every input must decode to either `Ok(_)` or
`Err(LeanError::Host(stage = Conversion))`; any panic, exception kind, or
sanitizer-detected fault is a fuzzing finding.

### CI

The workspace's stable matrix (`.github/workflows/ci.yml`) is unchanged. A new dedicated
workflow `.github/workflows/sanitizer.yml` runs the local ASan command above on
`ubuntu-latest` with `nightly`, plus the fuzz target for 120 seconds, on every `push` to
`main`, every `pull_request`, and a weekly cron. The job is not allowed to fail; sanitizer
findings block the PR.

### Known gaps

- **macOS AddressSanitizer is not currently run in CI.** AddressSanitizer is available on
  `aarch64-apple-darwin` on nightly, but the interaction between Lean's runtime (`libleanrt`
  links its own mimalloc) and ASan's allocator-shim on macOS has not been validated. Adding a
  macOS ASan job is deferred to prompts 24 (concurrency) or 30 (release readiness); the gap
  is recorded here so a future session can pick it up without re-deriving the rationale.
- **Miri does not cover the Lean C runtime.** Miri can validate the pure-Rust seams in
  `lean-rs-sys` (refcount mirror's `AtomicI32::from_ptr`, layout casts in `repr` tests, the
  `NonNull` arithmetic in the inline helpers when fed mock pointers), but it cannot execute
  `libleanshared`. `docs/architecture/01-safety-model.md`'s safety-test guidance accepts this
  by enumerating sanitizers and stress tests as the alternative.
- **The host machine for prompt 23's verification is macOS arm64.** Steps 6 (ASan) and 7
  (fuzz) above run only on CI. The stable verification block in
  `00-current-state.md` for `SAFETY-HARDENING` records the macOS commit; the sanitizer run is
  cited via its GitHub Actions URL.

## Maintenance rule

When any commit changes an `unsafe { ... }` block or adds/removes a `pub unsafe fn`, the
matching entry in this document must be updated in the same commit. The CI clippy job catches
unsafe added without a `// SAFETY:` comment (via the `missing_safety_doc` lint at
`warn`); this document catches the inverse — `unsafe` whose invariant has drifted from what the
code actually relies on.
