# Unsafe Inventory

Every `unsafe` block and `pub unsafe fn` in the workspace, paired with two things: the caller's **precondition** (what
the call site must guarantee before invoking) and the block's **invariant** (what it promises in return). Audience:
auditors reviewing the safety story, and maintainers about to change an unsafe seam.

**Maintenance rule.** Any commit that adds, removes, or reshapes an `unsafe` block must update the matching row in this
file in the same commit. CI catches `unsafe` without a `// SAFETY:` comment (the `missing_safety_doc` lint at `warn`);
this file catches the inverse—invariants that drifted away from what the code actually relies on.

The thesis these invariants serve is in [`docs/architecture/01-safety-model.md`](../architecture/01-safety-model.md).
Raw `lean_*` symbols enter the workspace only through `lean-rs-sys`; `lean-rs` consumes them through narrow per-file
`#![allow(unsafe_code)]` opt-outs; `lean-rs-host` currently has trusted-boundary opt-outs that must be removed before
it is a safe consumer; `lean-toolchain` has no Rust `unsafe`.

## Safety boundary audit, 2026-05-27

This section classifies the production search surface requested for the pre-1.0 migration series:

```sh
rg -n "unsafe|lean_rs_sys|from_function_address|exported_unchecked<" crates/lean-rs crates/lean-rs-host crates/lean-toolchain
```

Tests, examples, fuzz targets, compile-fail stderr snapshots, and generated fixture/shim source are not counted as
production Rust below unless explicitly named. Lean-side `unsafe Lean.enableInitializersExecution` in bundled shims is a
Lean initialisation permission, not Rust memory-unsafe code; it is tracked as an unrelated host-shim invariant.

### Inventory by classification

| Classification | Production sites | Current status |
| --- | --- | --- |
| Lean runtime ownership/lifetime management that belongs in `lean-rs` | `lean-rs/src/runtime/{init,obj,thread}.rs`; `lean-rs/src/abi/traits.rs`; handle wrappers in `lean-rs/src/handle/{name,level,expr,declaration}.rs`; `lean-rs-host/src/host/session.rs` argument-only `LeanAbi` for `LeanDeclarationFilter`; `lean-rs-host/src/host/process/query.rs` argument-only `LeanAbi` impls for module-query request/budget/selector types | Correctly centralised in `lean-rs` except the host argument-only impls, which still import `lean_rs_sys::lean_object` and use `Obj::from_owned_raw` to drop impossible return values. Move this ownership/drop pattern behind `lean-rs` conversion support. |
| Lean object layout decoding that belongs behind safe `lean-rs` view APIs | `lean-rs/src/abi/{array,bytearray,except,int,nat,option,scalar,string,structure,tuple}.rs`; `lean-rs/src/error/io.rs`; `lean-rs/src/callback.rs`; `lean-rs-host/src/host/elaboration/{diagnostic,failure}.rs`; `lean-rs-host/src/host/evidence/status.rs`; `lean-rs-host/src/host/process/query.rs` | Expected inside `lean-rs`. `lean-rs::abi::structure::{ObjView, CtorView}` now centralises scalar/nullary tag, ctor-tag, ctor-arity, and scalar-tail reads for host migrations. Still leaked in `lean-rs-host` until those decoders move off direct `lean_rs_sys` imports. |
| Dynamic symbol dispatch that must become checked or explicitly unsafe | `lean-rs/src/module/library.rs`; `lean-rs/src/module/exported.rs`; every `LeanExported::from_function_address` use in `lean-rs-host/src/host/session.rs`, including mandatory session symbols, optional meta/process/cache symbols, `make_name`, and `call_capability_unchecked` | `lean-rs` owns `dlopen`, global-vs-function classification, `dlsym`, initializer lookup, and the unsafe function-pointer cast. `lean-rs-host` is trusted code today: it pins shim signatures in comments and constructs typed handles from raw addresses. Arbitrary `call_capability_unchecked` lookup is safe only by caller discipline today; it needs checked signature metadata or an explicitly unsafe API. |
| Callback or context-pointer handling that belongs behind a safe callback API | `lean-rs/src/callback.rs`; `lean-rs-host/src/host/progress.rs` | `lean-rs` owns the callback registry, status bytes, panic containment, and string/progress payload trampolines. `lean-rs-host` still casts a stack-owned progress context address back to a reference inside the registered closure. That synchronous lifetime rule should be represented by a safe callback/session-progress API rather than repeated by host code. |
| Unrelated unsafe that needs its own justification | `lean-rs/src/module/initializer.rs` `from_utf8_unchecked` for prevalidated symbol bytes and initializer call; `lean-rs/src/module/library.rs` platform loader calls including `RTLD_GLOBAL`; bundled Lean shims call `unsafe Lean.enableInitializersExecution` | Keep documented locally. These are not Lean object layout or Rust callback lifetime leaks, but each remains load-bearing and should stay isolated. |
| `lean_rs_sys` constants only, no unsafe | `lean-toolchain/src/{discover,fingerprint,lib,manifest_validation,modules}.rs`; selected docs/tests | Build-time version, digest, required-symbol, and supported-toolchain metadata. This is not raw object access and does not by itself expand the unsafe boundary. |

### Information leakage to remove

- **Lean object layout leakage:** `lean-rs` now owns scalar-vs-constructor tags and constructor scalar-tail reads through
  `ObjView` / `CtorView`, but `lean-rs-host` has not been migrated yet. Host still copies this rule in diagnostic
  severity, evidence status, module-query cache facts, timings, booleans, and sum tags.
- **Symbol ABI leakage:** `lean-rs-host/src/host/session.rs` duplicates the Lean signature of every shim export in the
  capability contract table and again at each typed `LeanExported::from_function_address` call site. The same ABI fact
  is present in Lean shim declarations and Rust comments/type annotations.
- **Callback lifetime leakage:** `lean-rs` owns callback handles and trampolines, but `lean-rs-host` still knows that a
  progress callback is synchronous and that the boxed context outlives exactly one Lean call.
- **Ownership/drop leakage:** host request-only `LeanAbi` impls need to know that an owned `lean_obj_res` must be wrapped
  and dropped if Lean ever returns it, even though these types are not valid return types.

## `lean-rs-sys`—the load-bearing boundary

`lean-rs-sys` is the one crate-wide `#[allow(unsafe_code)]` opt-out (`crates/lean-rs-sys/src/lib.rs:32`). `lean_object`
is opaque (`[u8; 0] + PhantomData<(*mut u8, PhantomPinned)>`) and downstream code reaches state only through
`pub unsafe fn` helpers. Each helper documents its caller obligations in a `# Safety` block; each in-body
`unsafe { ... }` restates the ABI/layout invariant it relies on.

The blanket allow is justified by two cross-checks:

- **`SUPPORTED_TOOLCHAINS` digest** pins `lean.h` bytes across every release in the supported window.
- **`REQUIRED_SYMBOLS` link-time test** confirms every symbol the inline helpers cast through is exported by
  `libleanshared` for every release in the window.

Each table below groups blocks by `pub unsafe fn`; the function-level `# Safety` doc is the authoritative statement, and
in-body blocks rely on the same caller obligations.

### `crates/lean-rs-sys/src/object.rs`—13 `pub unsafe fn`, 12 blocks

Object inspection, tagging, and runtime-mode reads. Mirrors `lean.h:312–630`.

| `pub unsafe fn` (line) | Precondition | Invariant |
| --- | --- | --- |
| `lean_is_scalar` (51) | `o` is any Lean object pointer | pointer-bit math, no deref |
| `lean_box` (63) | `n` fits the scalar payload width | result is a scalar-tagged non-null pointer |
| `lean_unbox` (74) | `o` is scalar-tagged (`lean_is_scalar(o)` is true) | pointer-bit math, no deref |
| `header` (82) | `o` is a live non-scalar Lean object | layout cast `*o.cast::<LeanObjectRepr>()`; layout pinned by `SUPPORTED_TOOLCHAINS` |
| `lean_alloc_object` extern wrapper (91) | `size` is non-zero | result is owned or null on OOM (Lean aborts internally) |
| `lean_ptr_tag` (103) | `o` is a live non-scalar object | reads `header(o).m_tag` |
| `lean_ptr_other` (114) | `o` is a live non-scalar object whose `m_other` byte is defined for its tag | reads `header(o).m_other` |
| `lean_is_*` predicates (macro-stamped) | `o` is a live non-scalar object | `lean_ptr_tag(o) == TAG` |
| `lean_is_ctor` (141) | `o` is a live non-scalar object | `lean_ptr_tag(o) <= LEAN_MAX_CTOR_TAG` |
| `lean_obj_tag` (197) | `o` is a live non-scalar constructor | reads `m_tag`, saturating to `u32` |
| `lean_is_st` / `_mt` / `_persistent` / `_exclusive` / `_shared` (214–259) | `o` is a live non-scalar object | refcount sign rules per `refcount.rs` module doc |

### `crates/lean-rs-sys/src/refcount.rs`—6 `pub unsafe fn`, 8 blocks

Refcount fast paths. The atomic operations go through `AtomicI32::from_ptr(&raw mut (*repr).m_rc)` (Rust 1.75+), so the
load/store/`fetch_sub` sites see a safe `&AtomicI32`.

| `pub unsafe fn` (line) | Precondition | Invariant |
| --- | --- | --- |
| `rc_atom` (private, ~38) | `o` is a live non-scalar object; layout pinned | `AtomicI32::from_ptr` over `(*LeanObjectRepr).m_rc` |
| `lean_inc_ref_n` (68) | `o` is a live non-scalar object; `n >= 1` | calls `lean_inc_ref_n_cold` for the MT slow path |
| `lean_inc_ref` (91) | as `lean_inc_ref_n` with `n = 1` | delegates to `lean_inc_ref_n` |
| `lean_inc` (105) | `o` is a live object (scalar or non-scalar) | scalar short-circuit; otherwise `lean_inc_ref` |
| `lean_inc_n` (122) | `o` is a live object; `n >= 1` | scalar short-circuit; otherwise `lean_inc_ref_n` |
| `lean_dec_ref` (148) | `o` is a live non-scalar object whose RC the caller transfers | calls `lean_dec_ref_cold` for MT/free path |
| `lean_dec` (172) | `o` is a live object whose RC the caller transfers | scalar short-circuit; otherwise `lean_dec_ref` |

### `crates/lean-rs-sys/src/ctor.rs`—24 `pub unsafe fn`, 36 blocks

Constructor objects, polymorphic boxing, per-width scalar getters and setters. Mirrors `lean.h:642–760` and
`lean.h:2811–2873`.

| `pub unsafe fn` (line) | Precondition | Invariant |
| --- | --- | --- |
| `lean_alloc_ctor` (121) | `tag <= LEAN_MAX_CTOR_TAG`; `num_objs <= 256`; `scalar_sz <= u16::MAX`; aligned object size <= `LEAN_MAX_SMALL_OBJECT_SIZE` | calls `lean_alloc_object`; writes `m_rc`, aligned `m_cs_sz`, `m_tag`, and `m_other` so refcounting, constructor arity, and object-size helpers see a well-formed small ctor; result owns one refcount |
| `lean_ctor_num_objs` (59) | `o` is a live ctor | reads `lean_ptr_other(o)` |
| `lean_ctor_obj_cptr` (72) | `o` is a live ctor with `num_objs` slots | layout cast to `LeanCtorObjectRepr.objs` |
| `lean_ctor_scalar_cptr` (86) | `o` is a live ctor whose scalar payload follows `num_objs * sizeof(ptr)` | layout cast past the object pointer array |
| `lean_box_uint32` / `_uint64` / `_usize` / `_float` (140–229) | `v` fits the boxed width | calls `lean_alloc_ctor(0, 0, sizeof(v))` + scalar write |
| `lean_unbox_uint32` / `_uint64` / `_usize` / `_float` (160–246) | `o` was produced by the matching `lean_box_*` | calls `lean_ctor_get_*(o, 0)` |
| `lean_ctor_get_usize` (259) | `o` is a ctor; `i` indexes a slot wide enough for `usize` | `lean_ctor_obj_cptr(o).add(i).cast::<usize>().read_unaligned()` |
| `lean_ctor_get_uint8` / `_16` / `_32` / `_64` (276–) | `o` is a ctor; `offset` is in-range and aligned for the ctor tag | `lean_ctor_scalar_cptr(o).add(offset).cast::<…>().read_unaligned()` |
| `lean_ctor_set_*` family | `i` / `offset` is in-range; object writes transfer one refcount | symmetric writes to scalar/object slots |

### `crates/lean-rs-sys/src/array.rs`—11 `pub unsafe fn`, 13 blocks

Object arrays (`Array α`) and scalar arrays (`ByteArray`, `FloatArray`, …). Mirrors `lean.h:815–1028`.

| `pub unsafe fn` (line) | Precondition | Invariant |
| --- | --- | --- |
| `lean_alloc_array` (44) | `capacity >= size`; both fit `usize` | `lean_alloc_object` + writes `{size, capacity}`; result owns one refcount with `size` pointer slots logically reserved |
| `lean_alloc_sarray` (82) | `elem_size > 0`; `capacity >= size` | `lean_alloc_object` + writes `{elem_size, size, capacity}`; payload bytes uninit until written |
| `as_array` (105, private) | `o` is a live array | layout cast `*o.cast::<LeanArrayObjectRepr>()` |
| `as_sarray` (111, private) | `o` is a live scalar-array | layout cast `*o.cast::<LeanSArrayObjectRepr>()` |
| `lean_array_size` (122) | `o` is a live array | reads `as_array(o).size` |
| `lean_array_capacity` (133) | as above | reads `as_array(o).capacity` |
| `lean_array_cptr` (145) | `o` is a live array | pointer past header; valid for `size` reads/writes |
| `lean_array_get_core` (157) | `i < lean_array_size(o)` | `*lean_array_cptr(o).add(i)`; returned pointer is a borrow (no RC transfer) |
| `lean_array_set_core` (169) | `i < lean_array_capacity(o)`; `v` owns one refcount transferred into the slot | `*lean_array_cptr(o).add(i) = v` |
| `lean_sarray_elem_size` / `lean_sarray_size` / `lean_sarray_capacity` / `lean_sarray_cptr` (180–214) | `o` is a live scalar-array | symmetric reads against `LeanSArrayObjectRepr`; `cptr` valid for `size * elem_size` bytes |

### `crates/lean-rs-sys/src/closure.rs`—7 `pub unsafe fn`, 8 blocks

Closure objects. Mirrors `lean.h:762–813`.

| `pub unsafe fn` (line) | Precondition | Invariant |
| --- | --- | --- |
| `lean_alloc_closure` (290) | `fun` is a valid function pointer expecting `arity` Lean args; `num_fixed <= arity` | `lean_alloc_object` + writes `{fun, arity, num_fixed}`; payload uninit until filled |
| `as_closure` (~200, private) | `o` is a live closure | layout cast |
| `lean_closure_fun` / `lean_closure_arity` / `lean_closure_num_fixed` (212–234) | `o` is a live closure | header reads |
| `lean_closure_arg_cptr` (246) | `o` is a live closure | pointer past header; valid for `num_fixed` reads/writes |
| `lean_closure_get` (257) | `i < num_fixed` | indexed read |
| `lean_closure_set` (269) | `i < num_fixed`; write transfers one refcount | indexed write |

### `crates/lean-rs-sys/src/string.rs`—5 `pub unsafe fn`, 6 blocks

String objects. Mirrors `lean.h:1157–1234`.

| `pub unsafe fn` (line) | Precondition | Invariant |
| --- | --- | --- |
| `as_string` (~38, private) | `o` is a live string (`lean_is_string(o)` is true) | layout cast `*o.cast::<LeanStringObjectRepr>()` |
| `lean_string_size` / `lean_string_len` / `lean_string_capacity` (48–70) | `o` is a live string | header reads |
| `lean_string_cstr` (82) | as above | pointer past header to NUL-terminated UTF-8; valid for `lean_string_size(o)` bytes including NUL |
| `lean_string_byte_size` (100) | as above | `size_of::<LeanStringObjectRepr>() + lean_string_capacity(o)` (saturating) |

### `crates/lean-rs-sys/src/scalar.rs`—12 `pub unsafe fn`, 12 blocks

Boxed-scalar conversions. Mirrors `lean.h:1356–2065`.

| `pub unsafe fn` (line) | Precondition | Invariant |
| --- | --- | --- |
| `lean_usize_to_nat` (29) |—| `lean_box(n)` if `n <= LEAN_MAX_SMALL_NAT`; else extern `lean_cstr_to_nat_*`; result owns one refcount |
| `lean_unsigned_to_nat` (47) |—| delegates to `lean_usize_to_nat` |
| `lean_uint64_to_nat` (59) |—| scalar fast path or `lean_cstr_to_nat` for the 64-bit overflow region |
| `lean_uint8_of_nat` (77) | `a` is a `Nat` (scalar or bignum) | `lean_obj_tag` + scalar / bignum dispatch; result truncates to `u8` |
| `lean_uint8_to_nat` / `_16` / `_32` (94–116) |—| widening to `usize` then `lean_usize_to_nat` |
| `lean_int_to_int` (128), `lean_int64_to_int` (150) | `n` fits the requested representation | scalar fast path or extern `lean_cstr_to_int`; result owns one refcount |
| `lean_nat_to_int` (168) | `a` is an owned `Nat` | extern coercion; result owns one refcount |
| `lean_scalar_to_int64` (189), `lean_scalar_to_int` (206) | `a` is a scalar-tagged `Int` | unbox + sign-extend |

### `crates/lean-rs-sys/src/io.rs`—5 `pub unsafe fn`, 6 blocks

`IO` result helpers. Mirrors `lean.h:2893–2907`.

| `pub unsafe fn` (line) | Precondition | Invariant |
| --- | --- | --- |
| `ctor_get0` (~20, private) | `r` is a live ctor with at least one object slot | `lean_ctor_obj_cptr(r).read()` |
| `lean_io_result_is_ok` (35), `lean_io_result_is_error` (46) | `r` is a live non-scalar `IO α` result (tag 0 = `ok`, tag 1 = `error`) | `lean_ptr_tag(r) == {0, 1}` |
| `lean_io_result_get_value` (59), `lean_io_result_get_error` (70) | as above | borrowed `ctor_get0(r)`; no RC transfer |
| `lean_io_result_take_value` (83) | `r` is an owned `IO α` result | move out then `lean_dec(r)`; caller owns the returned value's refcount |

### `crates/lean-rs-sys/src/external.rs`—3 `pub unsafe fn`, 3 blocks

External objects (C `void*` payloads). Mirrors `lean.h:295–1332`.

| `pub unsafe fn` (line) | Precondition | Invariant |
| --- | --- | --- |
| `lean_alloc_external` (35) | `cls` is a valid `lean_external_class` pointer for `data` | `lean_alloc_object` + writes `{class, data}` |
| `lean_get_external_class` (58), `lean_get_external_data` (70) | `o` is a live external object | header reads |

### Other `lean-rs-sys` files

- **`init.rs`**—0 blocks; 7 extern declarations (`lean_initialize`, `lean_initialize_runtime_module`,
  `lean_initialize_thread`, `lean_finalize_thread`, `lean_setup_args`, `lean_init_task_manager`,
  `lean_init_task_manager_using`, `lean_finalize_task_manager`). Calling any of them is `unsafe` per Lean's runtime
  entry-point rules: `lean_initialize` exactly once per process; `lean_initialize_thread` paired with
  `lean_finalize_thread` on every worker thread; `lean_setup_args` argv must outlive readers.
- **`nat_int.rs`**—0 blocks; bignum externs from `lean.h:1334–1853` (`lean_cstr_to_nat`, `lean_cstr_to_int`, bignum
  arithmetic helpers).
- **`repr.rs`**—1 block in a `#[cfg(test)]` layout assertion that the in-memory layout matches the bytes pinned by every
  `SUPPORTED_TOOLCHAINS` entry. Production paths never touch `repr` directly except through layout casts in the inline
  accessors of the sibling modules.
- **`lib.rs`**—1 block and 2 `pub unsafe fn`; the block forwards a discovery helper returning a `&'static` view of the
  `REQUIRED_SYMBOLS` table. The `unsafe` is the caller's invariant that the link-time test has succeeded.
- **`consts.rs`**, **`types.rs`**—0 blocks. `consts.rs` holds the `build.rs`-resolved version and digest constants.
  `types.rs` defines opaque `lean_object` and calling-convention typedefs; its one `pub unsafe fn` is a `Default`-like
  constructor stub with no body that callers cannot reach.

---

## `lean-rs`—per-file opt-outs

Every block below ultimately calls a `pub unsafe fn` from `lean-rs-sys`; the invariant is what that `# Safety` doc
requires, satisfied by the call site's local context. Per-file `#![allow(unsafe_code)]` keeps the opt-out as narrow as
the safety model demands.

### Runtime—`crates/lean-rs/src/runtime/`

#### `runtime/obj.rs`—19 blocks (`#![allow(unsafe_code)]` at line 20)

Owned (`Obj<'lean>`) and borrowed (`ObjRef<'lean, 'a>`) handles. Six production blocks plus test-module RC observations.

| Line | Block | Calls (`lean-rs-sys`) | Precondition |
| --- | --- | --- | --- |
| 91 | `Obj::from_owned_raw` non-null wrap | `NonNull::new_unchecked` | caller-documented non-null + one-refcount transfer |
| 148 | `Obj::runtime` ZST borrow synthesis | `NonNull::dangling().as_ref()` | `LeanRuntime` is zero-sized; `'lean` witnesses initialisation |
| 167 | `Clone for Obj::clone` | `lean_inc` | `self.ptr` is a live owned object (refcount ≥ 1) |
| 182 | `Drop for Obj::drop` | `lean_dec` | `self.ptr` owns exactly one refcount about to be released |
| 229 | test `scalar_obj` | `lean_box` | `7` fits scalar payload |
| 239 | test `heap_string` | `lean_mk_string` | `c"abc"` is a valid NUL-terminated UTF-8 cstring |
| 258, 262, 263, 267 | `clone_increments_heap_refcount` predicates | `lean_is_exclusive`, `lean_is_shared` | header-only inspection of live owned object |
| 277, 283, 288, 290 | `into_raw_does_not_decrement` body | `lean_is_shared`, `lean_dec` | header-only predicates; `lean_dec` releases the count from `into_raw` |
| 298, 305, 311 | `borrow_does_not_adjust_refcount` predicates | `lean_is_exclusive` | header-only inspection |
| 378 | `_lifetime_anchored_to_runtime_borrow` sentinel | `lean_box` | scalar pointer arithmetic |

#### `runtime/init.rs`—3 blocks (`#![allow(unsafe_code)]` at line 14)

`LeanRuntime::init` calls `lean_initialize_runtime_module`, `lean_initialize`, and `lean_init_task_manager`. The triple
block at line 106 carries one `// SAFETY:` comment covering all three: process-once initialisation, sequenced as Lake's
compiler expects.

Line 128—`unsafe { NonNull::<LeanRuntime>::dangling().as_ref() }`—synthesises the ZST `&LeanRuntime` from the runtime
cell pointer; sound because `LeanRuntime` is zero-sized and the caller has just verified the cell is initialised.

#### `runtime/thread.rs`—3 blocks (`#![allow(unsafe_code)]` at line 13)

`LeanThreadGuard::attach` calls `lean_initialize_thread`; `Drop` calls `lean_finalize_thread`. The blocks at lines 67
and 84 each pair an `attach` / `finalize` on the **same** OS thread, guarded by an RAII handle that cannot be `Send`
(the FFI calls require thread-local Lean state).

### ABI—`crates/lean-rs/src/abi/`

Every block in this directory either (a) wraps a freshly-allocated Lean value as a fresh `Obj<'lean>` via
`Obj::from_owned_raw` (sys symbol is the matching `lean_alloc_*` or `lean_*_to_nat`), or (b) inspects a borrowed
object's header via a sys predicate (`lean_is_scalar`, `lean_is_ctor`, `lean_obj_tag`, `lean_ctor_num_objs`, …).

| File | Blocks | Opt-out | Calls (`lean-rs-sys`) | Notes |
| --- | ---: | --- | --- | --- |
| `abi/scalar.rs` | 22 | `#![allow(unsafe_code)]` line 26 | `lean_box*` / `lean_unbox*` + predicates | per-block `// SAFETY:` is "pointer-bit math" or "boxed above; layout pinned" |
| `abi/nat.rs` | 8 | line 17 | `lean_uint64_to_nat`, `lean_usize_to_nat`, `lean_unbox`, `lean_is_scalar`, `lean_obj_tag` | bignum branches return `Conversion` error rather than read the MPZ payload |
| `abi/int.rs` | 4 | line 10 | `lean_int64_to_int`, `lean_scalar_to_int64`, `lean_is_scalar`, `lean_obj_tag` |—|
| `abi/string.rs` | 11 | line 20 | `lean_mk_string`, `lean_is_string`, `lean_string_cstr`, `lean_string_len`, `lean_is_scalar`, `lean_obj_tag` | slice constructions at lines 100 and 174 carry lifetime-bound `// SAFETY:` (`'a` tied to source `ObjRef`) |
| `abi/bytearray.rs` | 9 | line 22 | `lean_alloc_sarray`, `lean_sarray_elem_size`, `lean_sarray_cptr`, `lean_sarray_size`, `lean_is_scalar`, `lean_is_sarray`, `lean_obj_tag` | alloc block (line 45) writes payload with the same `elem_size = 1` precondition the read side checks |
| `abi/array.rs` | 9 | line 28 | `lean_alloc_array`, `lean_array_size`, `lean_array_cptr`, `lean_array_set_core`, `lean_array_get_core`, `lean_is_array`, `lean_is_scalar`, `lean_obj_tag` | `from_iter_exact` write loop (line 56) relies on `lean_alloc_array(n, n)` so every slot is written exactly once |
| `abi/option.rs` | 8 | line 27 | `lean_box(0)` for `None`, `lean_is_scalar`, `lean_unbox`, `lean_is_ctor`, `lean_obj_tag` | encodes Lean's mixed-arity nullary-scalar `Option` |
| `abi/except.rs` | 2 | per-block | `Obj::from_owned_raw` (×2) | invariant is "`c` is a `lean_obj_res` owning one refcount per Lake's contract"—established by the typed function-pointer cast in `LeanExported::call` |
| `abi/structure.rs` | 24 | line 38 | `lean_alloc_ctor`, `lean_ctor_obj_cptr`, `lean_ctor_scalar_cptr`, `lean_ctor_num_objs`, `lean_ctor_get_uint8`, `lean_ctor_get_uint64`, `lean_object_data_byte_size`, `lean_is_scalar`, `lean_is_ctor`, `lean_obj_tag`, `lean_unbox` | `ObjView` / `CtorView` perform borrow-only shape, tag, and scalar-tail reads with explicit bounds checks; `take_ctor_objects::<N>` reads field slots and pairs each `lean_inc` with the parent drop so returned `Obj`s own their refcounts |
| `abi/traits.rs` | 1 | per-block line 162 | `Obj::from_owned_raw` | blanket `LeanAbi for Obj<'lean>` identity impl |
| `abi/tests.rs` | 2 | per-block line 582 |—| borrowed-view pointer-equality assertion; no header deref |

### Module—`crates/lean-rs/src/module/`

#### `module/library.rs`—6 blocks (`#![allow(unsafe_code)]` at line 32)

Calls `libloading::Library::new` (line 112) and `library.get` (lines 186, 212, 233, 253). All `unsafe` for dlopen-time
reasons: the loaded library may run constructors, and resolved symbols are typed by the caller.

#### `module/initializer.rs`—4 blocks (`#![allow(unsafe_code)]` at line 34)

Calls a Lake-emitted module initializer function pointer (line 198, wrapped in `catch_unwind`) and wraps the returned
`IO α` result as an `Obj` (line 218). Block 116 is a `from_utf8_unchecked` on bytes already validated by the
symbol-bytes builder.

#### `module/exported.rs`—7 blocks (`#![allow(unsafe_code)]` at line 59)

The typed `LeanExported::call` machinery. Each block at lines 246, 310, 314, 473, 489, 512, 517 either wraps a
freshly-returned `lean_object*` as an `Obj`, transmutes between `R::CRepr` and `*mut lean_object` (line 517—sound
because `R: LeanAbi` constrains `CRepr` to either a scalar primitive or `*mut lean_object`), or dispatches the
function-pointer call through the per-arity macro.

### Fuzzing entry—`crates/lean-rs/src/fuzz_entry.rs`

Feature-gated by `fuzzing` (off by default, not semver-stable). Seven `pub unsafe fn` wrappers (`decode_string`,
`decode_bytearray`, `decode_array_u64`, `decode_option_u64`, `decode_except`, `decode_nat_u64`, `decode_ctor_tag`) plus
seven matching inner blocks. Each takes a `*mut lean_object` owning one transferred refcount and wraps it in an
`Obj<'lean>` via `unsafe { Obj::from_owned_raw(runtime, raw) }`; the invariant is the same as `Obj::from_owned_raw`'s
`# Safety` doc, discharged by the fuzz harness constructing inputs through `lean-rs-sys`'s public allocators. The module
sits at the crate root so the `pub(crate) abi` boundary stays intact when the feature is off.

### Error—`crates/lean-rs/src/error/`

#### `error/io.rs`—16 blocks (`#![allow(unsafe_code)]` at line 28)

The `IO α` result decoder. Blocks at lines 64, 69, 73, 80, 125, 136, 153, 158, 163, 165, 169, 175 read the `IO.Error`
constructor's fields via `lean_io_result_*`, `lean_obj_tag`, `lean_ctor_num_objs`, `lean_ctor_obj_cptr`,
`lean_is_scalar`, `lean_is_string`, `lean_string_cstr`/`lean_string_len`. The test-support block at line 247 transmutes
a resolved `dlsym` address into a typed function pointer.

#### `error/panic.rs`—0 blocks

`catch_callback_panic` is pure safe Rust around `std::panic::catch_unwind`.

### Host—`crates/lean-rs/src/host/`

#### `host/session.rs`—16 blocks (`#![allow(unsafe_code)]` at line 89)

Each `LeanSession` method that dispatches into a Lake-installed function constructs a typed `LeanExported` via
`unsafe { LeanExported::from_function_address(runtime, address) }`. The address comes from `SessionSymbols`, populated
by one `dlsym` per symbol at capability load. `from_function_address`'s `# Safety` requires the address to have been
resolved as the correct typed symbol; `SessionSymbols::resolve` is the one place that obligation is discharged.

#### `host/handle/{name,level,expr,declaration}.rs`—1 block each (per-block)

Each constructs the public handle's inner `Obj` from a freshly-returned `lean_object*` produced by a fixture export.
Invariant is the `LeanExported::call` return contract.

#### `host/elaboration/failure.rs`—2 blocks (`#![allow(unsafe_code)]` at line 14)

`lean_ctor_get_uint8` reads the `Severity` byte off the failure ctor.

#### `host/elaboration/diagnostic.rs`—5 blocks (`#![allow(unsafe_code)]` at line 25)

`lean_ctor_get_uint8`, `lean_is_scalar`, `lean_unbox`, `lean_obj_tag` reads on the diagnostic ctor and its severity tag.

#### `host/evidence/handle.rs`—1 block (per-block)

Wraps the evidence handle's `Obj` from a fixture-returned pointer.

#### `host/evidence/status.rs`—2 blocks (per-block, lines 103 and 108)

Reads the `EvidenceStatus` scalar tag (`lean_is_scalar` + `lean_unbox`), with a heap-ctor fallback gated by
`lean_obj_tag` (not currently triggered but kept for forward-compat with a Lean representation change).

---

## `lean-toolchain`—0 unsafe blocks

`rg -n "unsafe" crates/lean-toolchain/src` returns nothing. The crate is build-time-only (toolchain discovery,
fingerprinting, link diagnostics) and consumes `lean-rs-sys` constants through their safe re-export. Any new `unsafe`
here would require a `#![allow(unsafe_code)]` opt-out with reviewer sign-off per
[`docs/architecture/01-safety-model.md`](../architecture/01-safety-model.md).

---

## Sanitizer and leak-check coverage

### Local—AddressSanitizer on Linux nightly

```sh
rustup toolchain install nightly --component rust-src
RUSTFLAGS="-Z sanitizer=address -Cdebug-assertions=on" \
RUSTDOCFLAGS="-Z sanitizer=address" \
cargo +nightly test -p lean-toolchain --target x86_64-unknown-linux-gnu \
  -- --test-threads=1
```

`-Z sanitizer=address` instruments pure-Rust helper code. CI intentionally does not run in-process Lean runtime tests
under Linux ASan: Lean's shared runtime is not ASan-instrumented and currently crashes the sanitizer before it can
produce actionable Rust diagnostics. The in-process Lean paths are instead covered by normal workspace tests, loader
regressions, worker crash-containment tests, and the release/manual ABI fuzz target. Loader-negative-path regressions
also run outside ASan because they intentionally exercise platform dynamic-loader failure behavior with uninstrumented
Lean shared libraries. `--test-threads=1` preserves the per-thread Lean runtime invariant for the helper checks that do
touch runtime discovery.

### Local—fuzz target (Linux or macOS, nightly)

```sh
rustup toolchain install nightly --component rust-src
cargo install cargo-fuzz                 # one-shot
(cd crates/lean-rs/shims/lean-rs-interop-shims && lake build)
(cd fixtures/lean && lake build)
cd crates/lean-rs/fuzz
cargo +nightly fuzz run abi_decode -- \
  -runs=200000 -max_total_time=120
```

`abi_decode` drives `lean_rs::abi`'s `{string, bytearray, array, option, except, structure}` decoders with
`Arbitrary`-generated Lean-shaped inputs constructed via `lean-rs-sys`'s public helpers. Every input must decode to
`Ok(_)` or `Err(LeanError::Host(stage = Conversion))`; any panic, exception kind, or sanitizer-detected fault is a
finding.

### CI

A dedicated workflow at `.github/workflows/sanitizer.yml` runs the Rust-only Linux ASan command above on `ubuntu-latest`
nightly and runs loader regressions outside ASan. The ASan job runs on every push to `main`, every pull request, a
weekly cron, and manual dispatch. The ABI fuzz smoke is manual-only in the sanitizer workflow and is a release gate in
`.github/workflows/release.yml`; it is not part of routine push/PR CI. The stable workspace matrix at
`.github/workflows/ci.yml` is unchanged.

### Coverage gaps

- **macOS AddressSanitizer is not yet run in CI.** ASan is available on `aarch64-apple-darwin` nightly, but the
  interaction between Lean's runtime (`libleanrt` links its own mimalloc) and ASan's allocator-shim on macOS has not
  been validated. Open future work.
- **Miri does not cover the Lean C runtime.** Miri can validate the pure-Rust seams in `lean-rs-sys` (refcount mirror's
  `AtomicI32::from_ptr`, layout casts in `repr` tests, `NonNull` arithmetic on mock pointers), but it cannot execute
  `libleanshared`. The safety-test guidance in
  [`docs/architecture/01-safety-model.md`](../architecture/01-safety-model.md) accepts this by naming sanitizers and
  stress tests as the alternative.
