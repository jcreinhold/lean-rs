# Unsafe inventory

This is the review map for unsafe production boundaries. Function-level `# Safety` documentation and local `// SAFETY:`
comments state the precise obligations; the tables here group those obligations by implementation owner. Update the
affected entry when a change adds, removes, or reshapes unsafe code. The workspace lint `missing_safety_doc` checks
public unsafe function documentation; it does **not** verify safety comments on blocks.

The [safety model](../architecture/safety.md) defines the policy. Raw `lean_*` symbols enter only through
`lean-rs-sys`. `lean-rs` discharges their runtime, object, ABI, loader, and callback obligations. `lean-rs-host`,
`lean-rs-abi`, and `lean-rs-worker-protocol` forbid unsafe code; `lean-toolchain` and `lean-rs-worker-parent` deny it
with narrow native-process/descriptor opt-outs.

To locate the current surface, inspect declarations and blocks, then distinguish production code from tests, comments,
examples, and the fuzz harness:

```sh
rg -n '\bunsafe\b|allow[(]unsafe_code' crates/lean-rs-sys/src crates/lean-rs/src \
  crates/lean-rs-host/src crates/lean-toolchain/src crates/lean-rs-worker-*/src
```

Lean-side `unsafe Lean.enableInitializersExecution` is a separate initialization permission, not Rust memory-unsafe
code. Static symbols, toolchain-window entries, and layout metadata belong to `lean-rs-abi`; discovery uses them without
raw Lean object access.

## `lean-rs-sys`—the load-bearing boundary

`lean-rs-sys` is the one crate-wide `#[allow(unsafe_code)]` opt-out in `crates/lean-rs-sys/src/lib.rs`. `lean_object` is
opaque (`[u8; 0] + PhantomData<(*mut u8, PhantomPinned)>`) and downstream code reaches state only through
`pub unsafe fn` helpers. Each helper documents its caller obligations in a `# Safety` block; each in-body
`unsafe { ... }` restates the ABI/layout invariant it relies on.

The blanket allow is justified by two cross-checks:

- **`SUPPORTED_TOOLCHAINS` digest** pins `lean.h` bytes across every release in the supported window.
- **`REQUIRED_SYMBOLS` link-time test** confirms every symbol the inline helpers cast through is exported by
  `libleanshared` for every release in the window.

Each table below groups blocks by `pub unsafe fn`; the function-level `# Safety` doc is the authoritative statement, and
in-body blocks rely on the same caller obligations.

### `crates/lean-rs-sys/src/object.rs`

Object inspection, tagging, and runtime-mode reads. Mirrors `lean.h:312–630`.

| Function | Precondition | Invariant |
| --- | --- | --- |
| `lean_is_scalar` | `o` is any Lean object pointer | pointer-bit math, no deref |
| `lean_box` | `n` fits the scalar payload width | result is a scalar-tagged non-null pointer |
| `lean_unbox` | `o` is scalar-tagged (`lean_is_scalar(o)` is true) | pointer-bit math, no deref |
| `header` | `o` is a live non-scalar Lean object | layout cast `*o.cast::<LeanObjectRepr>()`; layout pinned by `SUPPORTED_TOOLCHAINS` |
| `lean_alloc_object` extern wrapper | `size` is non-zero | result is owned; Lean aborts on allocation failure |
| `lean_ptr_tag` | `o` is a live non-scalar object | reads `header(o).m_tag` |
| `lean_ptr_other` | `o` is a live non-scalar object whose `m_other` byte is defined for its tag | reads `header(o).m_other` |
| `lean_is_*` predicates (macro-stamped) | `o` is a live non-scalar object | `lean_ptr_tag(o) == TAG` |
| `lean_is_ctor` | `o` is a live non-scalar object | `lean_ptr_tag(o) <= LEAN_MAX_CTOR_TAG` |
| `lean_obj_tag` | `o` is scalar-tagged or a live heap object | returns the unboxed scalar payload cast to `u32`, or the heap tag |
| `lean_is_st` / `_mt` / `_persistent` / `_exclusive` / `_shared` | `o` is a live non-scalar object | refcount sign rules per `refcount.rs` module doc |

### `crates/lean-rs-sys/src/refcount.rs`

Refcount fast paths. The atomic operations go through `AtomicI32::from_ptr(&raw mut (*repr).m_rc)` (Rust 1.75+), so the
load/store/`fetch_sub` sites see a safe `&AtomicI32`.

| Function | Precondition | Invariant |
| --- | --- | --- |
| `rc_atom` | `o` is a live non-scalar object; layout pinned | `AtomicI32::from_ptr` over `(*LeanObjectRepr).m_rc` |
| `lean_inc_ref_n` | `o` is a live non-scalar object; `n >= 1` | calls `lean_inc_ref_n_cold` for the MT slow path |
| `lean_inc_ref` | as `lean_inc_ref_n` with `n = 1` | delegates to `lean_inc_ref_n` |
| `lean_inc` | `o` is a live object (scalar or non-scalar) | scalar short-circuit; otherwise `lean_inc_ref` |
| `lean_inc_n` | `o` is a live object; `n >= 1` | scalar short-circuit; otherwise `lean_inc_ref_n` |
| `lean_dec_ref` | `o` is a live non-scalar object whose RC the caller transfers | calls `lean_dec_ref_cold` for MT/free path |
| `lean_dec` | `o` is a live object whose RC the caller transfers | scalar short-circuit; otherwise `lean_dec_ref` |

### `crates/lean-rs-sys/src/ctor.rs`

Constructor objects, polymorphic boxing, per-width scalar getters and setters. Mirrors `lean.h:642–760` and
`lean.h:2811–2873`.

| Function | Precondition | Invariant |
| --- | --- | --- |
| `lean_alloc_ctor` | `tag <= LEAN_MAX_CTOR_TAG`; `num_objs` fits the `u8` field; `scalar_sz <= u16::MAX`; aligned object size <= `LEAN_MAX_SMALL_OBJECT_SIZE` | calls `lean_alloc_object`; writes `m_rc`, aligned `m_cs_sz`, `m_tag`, and `m_other` so refcounting, constructor arity, and object-size helpers see a well-formed small ctor; result owns one refcount |
| `lean_ctor_num_objs` | `o` is a live ctor | reads `lean_ptr_other(o)` |
| `lean_ctor_obj_cptr` | `o` is a live ctor with `num_objs` slots | layout cast to `LeanCtorObjectRepr.objs` |
| `lean_ctor_scalar_cptr` | `o` is a live ctor whose scalar payload follows `num_objs * sizeof(ptr)` | layout cast past the object pointer array |
| `lean_box_uint32` / `_uint64` / `_usize` / `_float` | `v` fits the boxed width | calls `lean_alloc_ctor(0, 0, sizeof(v))` + scalar write |
| `lean_unbox_uint32` / `_uint64` / `_usize` / `_float` | `o` was produced by the matching `lean_box_*` | calls `lean_ctor_get_*(o, 0)` |
| `lean_ctor_get_usize` | `o` is a ctor; `i` indexes a slot wide enough for `usize` | `lean_ctor_obj_cptr(o).add(i).cast::<usize>().read_unaligned()` |
| `lean_ctor_get_uint8` / `_16` / `_32` / `_64` | `o` is a ctor; `offset` is in-range and aligned for the ctor tag | `lean_ctor_scalar_cptr(o).add(offset).cast::<…>().read_unaligned()` |
| `lean_ctor_set_*` family | `i` / `offset` is in-range; object writes transfer one refcount | symmetric writes to scalar/object slots |

### `crates/lean-rs-sys/src/array.rs`

Object arrays (`Array α`) and scalar arrays (`ByteArray`, `FloatArray`, …). Mirrors `lean.h:815–1028`.

| Function | Precondition | Invariant |
| --- | --- | --- |
| `lean_alloc_array` | `capacity >= size`; allocation size does not overflow; first `size` owned slots are initialized before observation | `lean_alloc_object` + writes `{size, capacity}`; result owns one refcount with `size` pointer slots logically reserved |
| `lean_alloc_sarray` | `elem_size` is one of `{1,2,4,8}` and below `0x80`; `capacity >= size`; allocation size does not overflow; payload initialized before observation | `lean_alloc_object` + writes `{elem_size, size, capacity}`; payload bytes uninit until written |
| `as_array` | `o` is a live array | layout cast `*o.cast::<LeanArrayObjectRepr>()` |
| `as_sarray` | `o` is a live scalar-array | layout cast `*o.cast::<LeanSArrayObjectRepr>()` |
| `lean_array_size` | `o` is a live array | reads `as_array(o).size` |
| `lean_array_capacity` | as above | reads `as_array(o).capacity` |
| `lean_array_cptr` | `o` is a live array | pointer past header; valid for `size` reads/writes |
| `lean_array_get_core` | `i < lean_array_size(o)` | `*lean_array_cptr(o).add(i)`; returned pointer is a borrow (no RC transfer) |
| `lean_array_set_core` | `i < lean_array_capacity(o)`; `v` owns one refcount transferred into the slot | `*lean_array_cptr(o).add(i) = v` |
| `lean_sarray_elem_size` / `lean_sarray_size` / `lean_sarray_capacity` / `lean_sarray_cptr` | `o` is a live scalar-array | symmetric reads against `LeanSArrayObjectRepr`; `cptr` valid for `size * elem_size` bytes |

### `crates/lean-rs-sys/src/closure.rs`

Closure objects. Mirrors `lean.h:762–813`.

| Function | Precondition | Invariant |
| --- | --- | --- |
| `lean_alloc_closure` | `fun` is a valid function pointer expecting `arity` Lean args; `num_fixed <= arity` | `lean_alloc_object` + writes `{fun, arity, num_fixed}`; payload uninit until filled |
| `as_closure` | `o` is a live closure | layout cast |
| `lean_closure_fun` / `lean_closure_arity` / `lean_closure_num_fixed` | `o` is a live closure | header reads |
| `lean_closure_arg_cptr` | `o` is a live closure | pointer past header; valid for `num_fixed` reads/writes |
| `lean_closure_get` | `i < num_fixed` | indexed read |
| `lean_closure_set` | `i < num_fixed`; write transfers one refcount | indexed write |

### `crates/lean-rs-sys/src/string.rs`

String objects. Mirrors `lean.h:1157–1234`.

| Function | Precondition | Invariant |
| --- | --- | --- |
| `as_string` | `o` is a live string (`lean_is_string(o)` is true) | layout cast `*o.cast::<LeanStringObjectRepr>()` |
| `lean_string_size` / `lean_string_len` / `lean_string_capacity` | `o` is a live string | header reads |
| `lean_string_cstr` | as above | pointer past header to NUL-terminated UTF-8; valid for `lean_string_size(o)` bytes including NUL |
| `lean_string_byte_size` | as above | `size_of::<LeanStringObjectRepr>() + lean_string_capacity(o)` (saturating) |

### `crates/lean-rs-sys/src/scalar.rs`

Boxed-scalar conversions. Mirrors `lean.h:1356–2065`.

| Function | Precondition | Invariant |
| --- | --- | --- |
| `lean_usize_to_nat` |—| `lean_box(n)` if `n <= LEAN_MAX_SMALL_NAT`; else extern `lean_big_usize_to_nat`; result owns one refcount |
| `lean_unsigned_to_nat` |—| delegates to `lean_usize_to_nat` |
| `lean_uint64_to_nat` |—| scalar fast path or `lean_big_uint64_to_nat` for values outside the small-Nat range |
| `lean_uint8_of_nat` | `a` is a `Nat` (scalar or bignum) | `lean_is_scalar` selects unboxing or `lean_uint8_of_big_nat`; result narrows to `u8` |
| `lean_uint8_to_nat` / `_16` / `_32` |—| widening to `usize` then `lean_usize_to_nat` |
| `lean_int_to_int`, `lean_int64_to_int` | `n` fits the requested representation | scalar fast path or matching `lean_big_int_to_int` / `lean_big_int64_to_int` extern; result owns one refcount |
| `lean_nat_to_int` | `a` is an owned `Nat` | scalar fast path or `lean_big_size_t_to_int`; consumed/result reference ownership follows the documented branch |
| `lean_scalar_to_int64`, `lean_scalar_to_int` | `a` is a scalar-tagged `Int` | unbox + sign-extend |

### `crates/lean-rs-sys/src/io.rs`

`IO` result helpers. Mirrors `lean.h:2893–2907`.

| Function | Precondition | Invariant |
| --- | --- | --- |
| `ctor_get0` | `r` is a live ctor with at least one object slot | `lean_ctor_obj_cptr(r).read()` |
| `lean_io_result_is_ok`, `lean_io_result_is_error` | `r` is a live non-scalar `IO α` result (tag 0 = `ok`, tag 1 = `error`) | `lean_ptr_tag(r) == {0, 1}` |
| `lean_io_result_get_value`, `lean_io_result_get_error` | as above | borrowed `ctor_get0(r)`; no RC transfer |
| `lean_io_result_take_value` | `r` is an owned `IO α` result | move out then `lean_dec(r)`; caller owns the returned value's refcount |

### `crates/lean-rs-sys/src/external.rs`

External objects (C `void*` payloads). Mirrors `lean.h:295–1332`.

| Function | Precondition | Invariant |
| --- | --- | --- |
| `lean_alloc_external` | `cls` is a valid `lean_external_class` pointer for `data` | `lean_alloc_object` + writes `{class, data}` |
| `lean_get_external_class`, `lean_get_external_data` | `o` is a live external object | header reads |

### Other `lean-rs-sys` files

- `init.rs`, `io.rs`, and `memory.rs` declare runtime entry points. Initialization is process-once; thread attach and
  finalize pair on the same OS thread; argument storage must outlive its readers. The memory-limit setter mutates
  process-global state and can lead to a fatal foreign exception on overrun.
- `nat_int.rs` declares bignum operations with the ownership and representation obligations documented at each extern.
- `repr.rs` holds crate-private layout mirrors and layout tests. Layout literals and supported header digests come from
  `lean-rs-abi`.
- `lib.rs` re-exports raw categories and safe static metadata queries. `consts.rs` exposes build-resolved identity;
  `types.rs` defines opaque `lean_object` and argument/result ownership aliases. They do not manufacture Lean objects.

## `lean-rs`—per-file opt-outs

Lean object operations discharge the raw bindings' obligations locally. Other blocks handle native loading, callback
context pointers, or zero-sized runtime references. Keep each unsafe opt-out at the narrow boundary that owns it.

### Runtime — `crates/lean-rs/src/runtime/`

| File | Precondition | Invariant |
| --- | --- | --- |
| `obj.rs` | `from_owned_raw` receives a non-null live Lean value with one transferred reference count. | `Obj` owns that count, `Clone` increments it, and `Drop` decrements it; borrowed views do not transfer ownership. The lifetime witnesses initialized runtime state. |
| `init.rs` | Initialization precedes all Lean work; process-once sequencing is owned by `OnceLock`. | Runtime-module initialization, Lean initialization, end-of-initialization marking, and task-manager startup run once. The static runtime reference uses a valid aligned non-null pointer to a zero-sized type. |
| `thread.rs` | Foreign OS-thread attachment and finalization occur on the same thread. | The non-Send RAII guard pairs native attach/finalize when it owns attachment. `init()` on a later thread marks local permission; it does not rerun native process initialization. |
| `memory.rs` | The hidden sibling bridge configures only an isolated worker/profiling process. | The setter changes Lean's process-global limit; it does not reclaim memory or promise a recoverable Rust error on overrun. |

### ABI—`crates/lean-rs/src/abi/`

ABI implementations encode owned arguments and decode owned results using the declared `CRepr`. Borrowed object
inspection keeps the source owner live. The signature metadata and implementation must agree on scalar width,
constructor shape, and owned/borrowed conventions.

| Files | Required shape or ownership | Invariant |
| --- | --- | --- |
| `scalar.rs` | Matching primitive width and scalar/boxed convention. | Box/unbox helpers use the corresponding ABI representation; decoded `Char` values are checked. |
| `nat.rs`, `int.rs` | A matching Lean `Nat` or `Int`. | Scalar fast paths use the tagged representation; unsupported large conversions return conversion errors rather than inspecting arbitrary MPZ payload. |
| `string.rs` | Live Lean string, checked before payload access. | Byte slices borrow the source object, size bounds are checked, and UTF-8 decoding does not outlive that borrow. |
| `bytearray.rs` | Scalar array with element size one. | Allocation fills the promised bytes; reads use the checked length and preserve the parent lifetime. |
| `array.rs` | Live object array with valid size/capacity and initialized slots. | Construction writes every slot; reads borrow fields or acquire a reference before returning an owned object. |
| `option.rs` | Lean's nullary scalar `none` or constructor `some` with the expected field. | Tag and field shape determine which payload may be read. |
| `except.rs` | Owned result from the corresponding typed Lean export. | Wrapping transfers one reference count into `Obj`; decoding follows the error/success constructor contract. |
| `structure.rs`, `tuple.rs` | Expected constructor tag, object arity, and scalar tail. | `ObjView`/`CtorView` check shape and scalar bounds; `take_ctor_objects` acquires child references before the parent is dropped. |
| `traits.rs` | The caller of `LeanAbi::from_c` supplies a valid value of the implementation's Lean type. | The unsafe `Obj` implementation transfers one owned reference into a handle tied to the runtime lifetime. |

`LeanCReprAbi` restricts C representations through a private sealing trait. Downstream code can implement `LeanAbi`
for a new Rust type with an `unsafe impl` and a supported C representation. Review each implementation for its actual
layout, ownership and scalar refinements. In particular, `Obj::from_c` delegates to `Obj::from_owned_raw`; the caller
establishes pointer validity and ownership before that call. The compile-fail cases `abi_c_representation_is_sealed`,
`abi_implementation_requires_unsafe` and `abi_raw_decode_requires_unsafe` exercise these distinct boundaries.

### Module—`crates/lean-rs/src/module/`

| File | Precondition | Invariant |
| --- | --- | --- |
| `module/library.rs` | A trusted native library and matching symbol kind. | `libloading` calls may run native constructors; function/global addresses remain tied to their loaded library. Dependency loading preserves the required symbol visibility. |
| `module/initializer.rs` | Correct Lake initializer name and ABI; validated symbol bytes. | The initializer result is wrapped with owned IO-result convention, and initializer ordering/idempotence remains tracked. `from_utf8_unchecked` is used only after byte validation. |
| `module/exported.rs` | Signature metadata or the unsafe caller establishes arity, primitive/boxed representations, ownership, and result convention. | Function-pointer casts and calls match that ABI; returned boxed values acquire one Rust owner. |
| `module/capability.rs` | Build manifests and signature metadata come from trusted capability authors. | Checked lookup compares declared signatures and preserves library/dependency lifetimes. Arbitrary dynamic lookup remains explicitly unsafe. |

### Fuzzing entry—`crates/lean-rs/src/fuzz_entry.rs`

The `fuzzing` feature (off by default, not semver-stable) exposes unsafe decode entry points for strings, byte arrays,
arrays, options, exceptions, naturals, and constructor tags. Each receives a pointer owning one transferred reference
and wraps it with `Obj::from_owned_raw`. The fuzz harness must construct a live input through the raw allocator helpers
and discharge the same validity/ownership preconditions as that constructor. The feature does not make malformed raw
pointers acceptable inputs.

### Error—`crates/lean-rs/src/error/`

`error/io.rs` requires the outer value to be a live owned `IO` result with the expected tag and payload shape,
established by the export ABI. The outer decoder trusts that precondition when reading its tag and first payload slot;
it does not validate arbitrary object shapes. Nested `IO.Error` message extraction checks constructor/string/scalar
shape and bounds while the source owner remains live. `error/panic.rs` uses safe `catch_unwind`; it does not catch C++
exceptions.

### Callbacks — `crates/lean-rs/src/callback.rs`

String payload pointers borrow a Lean `String` for the synchronous trampoline call. The callback checks scalar/string
shape before building a byte slice. Scoped progress context pointers refer to the stack-owned context only while the
synchronous registration is active; RAII unregisters the entry before that context can drop. User callback panics are
caught and converted to status bytes before returning to C. Callbacks must not outlive their registry entry or move
Lean-derived handles across threads.

### Host—`crates/lean-rs-host/src/host/`

`lean-rs-host` has `#![forbid(unsafe_code)]` at the crate root. Its host-shim dispatch is closed and checked:
`HostShimBindings` resolves bundled shim exports from manifest-backed signature metadata and stores typed handles for
the public `LeanSession` operations. Arbitrary user export dispatch is not rewrapped here.

#### Object-decoding host modules—0 blocks

`host/process/query.rs`, `host/elaboration/{diagnostic,failure}.rs`, and `host/evidence/{handle,status}.rs` no longer
import `lean_rs_sys` or opt into `unsafe_code`. They decode scalar tags, constructor tags, object-field arity, and
scalar-tail fields through `lean_rs::abi::structure::view` / `take_ctor_objects`, and they consume owned C-ABI results
through `Obj`'s `LeanAbi` implementation.

---

## Native process and descriptor boundaries

These APIs do not call raw Lean symbols. They use narrow platform FFI for process identity, private process groups,
accounting, and descriptor custody. This is an additional unsafe surface beyond the Lean object bridge.

| File | Precondition | Invariant |
| --- | --- | --- |
| `lean-toolchain/src/process_group.rs` | The owner retains its actual child and group identity until closure; inherited descriptor borrows outlive spawn. | `pre_exec` only performs async-signal-safe descriptor operations. macOS `waitid(WNOWAIT)` observes without reaping; fixed-size libproc buffers are checked. Signals target the still-owned private group. Passive stored-ID observations grant no signal authority. |
| `lean-toolchain/src/process_group/measured_child.rs` | `MeasuredChild` exclusively owns the actual unreaped child. | `wait4` output is read only after a terminal result for that exact PID; status/accounting have one reaping owner. |
| `lean-toolchain/src/inherited_pipe.rs` | Supplied private pipe descriptor exceeds standard I/O. | Numeric `fcntl` rejects invalid descriptors, preserves existing descriptor flags, and creates one unique CLOEXEC duplicate; shape/access/nonblocking checks precede use. |
| `lean-toolchain/src/shared_writer.rs` | Inherited loan metadata and stable lock identity validate. | A successful `fcntl` creates one uniquely owned duplicate without closing the supplied descriptor; file/device/inode and lock checks establish the accepted loan. |
| `lean-toolchain/src/native_guardian.rs` | The live guardian pins the worker session and remains outside the payload group. | Scalar scope queries validate each signal target; control descriptor adoption owns only a successful duplicate. |
| `lean-toolchain/src/native_guardian/ownership.rs` | A valid PID and adequately sized native output storage. | Kernel process snapshots validate returned size and identity; scalar SID/PGID observations retain no pointers and send no signals. |
| `lean-rs-worker-parent/src/pipe.rs` | The parent owns the pipe end throughout both calls. | `fcntl` preserves flags while enabling nonblocking I/O on that end. |
| `lean-rs-worker-parent/src/supervisor.rs` | Writable POD output buffers for macOS CPU accounting. | `proc_pidinfo` size and Mach timebase are checked before deriving milliseconds; failure yields unavailable accounting. |
| `lean-rs-worker-child/src/parent_watchdog.rs` | Fresh bootstrap before Lean starts. | `setsid` failure prevents runtime work; legacy lifeline EOF/error exits only the current process with `_exit`. Guardian cleanup has its separate ownership protocol. |
| `lean-rs-worker-child/src/child.rs` | Platform startup code acts only on the current process. | Signal handlers use async-signal-safe writes/exits; signal structures are initialized before installation; resource/core-dump and Linux parent-death settings affect this child. |

Test-only process-control opt-outs occur in native-guardian, measured-child, crash-ownership, and parent-watchdog
regressions. Synthetic object/layout tests and the fuzz entry points have separate input-generation obligations; they do
not expand the production API. Inspect their local safety comments when changing a harness.

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
ASAN_OPTIONS=detect_leaks=0 cargo +nightly fuzz run abi_decode -- \
  -runs=200000 -max_total_time=120
```

The existing Linux fuzz gate sets `ASAN_OPTIONS=detect_leaks=0` because Lean intentionally retains process-lifetime
allocations. This suppresses exit-time leak reports, not ABI bounds/ownership faults; macOS does not run LSan in this
configuration. Match the
[sanitizer workflow](https://github.com/jcreinhold/lean-rs/blob/main/.github/workflows/sanitizer.yml) when reproducing a
gate result.

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

### CI—Miri

The `miri` job in `.github/workflows/sanitizer.yml` runs
`MIRIFLAGS="-Zmiri-tree-borrows" cargo +nightly miri test -p lean-rs-sys` on every push to `main`, every pull request, a
weekly cron, and manual dispatch. It validates the pure-Rust unsafe that `lean-rs-sys` leans on—the refcount mirror's
`AtomicI32::from_ptr` fast paths (`single_threaded_inc_dec_round_trips`, `multi_threaded_inc_decrements_negated_count`,
`inc_ref_n_adds_in_place`) and the `o.cast::<LeanObjectRepr>()` header reads (`object::tests`)—over a synthetic,
Rust-owned `LeanObjectRepr` (`repr::test_support::MockObject`) that never crosses the refcount to zero, so the
`lean_dec_ref_cold` extern is never reached. The FFI-touching tests gate themselves out with
`#[cfg_attr(miri, ignore)]`. Strict provenance is intentionally **not** enabled: Lean's scalar encoding
(`lean_box`/`lean_unbox`) is an int-to-pointer cast by ABI, which `-Zmiri-strict-provenance` rejects by design. The job
needs only `LEAN_SYSROOT` (for the build-time header digest); it never executes the Lean runtime, so it skips the Lake
build and `LD_LIBRARY_PATH` setup.

### Coverage gaps

- **macOS AddressSanitizer is not yet run in CI.** ASan is available on `aarch64-apple-darwin` nightly, but the
  interaction between Lean's runtime (`libleanrt` links its own mimalloc) and ASan's allocator-shim on macOS has not
  been validated. Open future work.
- **Miri cannot execute the Lean C runtime.** The CI Miri lane (above) covers the pure-Rust boundary in `lean-rs-sys`
  (refcount mirror's `AtomicI32::from_ptr`, header-field casts, scalar-pointer encoding), but Miri cannot interpret
  `libleanshared`, so the live refcount discipline of `Obj`/`ObjRef`, callback unwinding, and thread-attach soundness
  remain out of its scope. Those paths are covered by the sanitizer, fuzz, and stress tests named in
  [`docs/architecture/safety.md`](../architecture/safety.md).
