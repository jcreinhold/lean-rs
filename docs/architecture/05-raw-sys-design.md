# `lean-rs-sys` Design Rationale

The per-decision rationale behind the `lean-rs-sys` crate's shape. The charter and
safety-model docs depend on it; the version-compatibility doc references it for the semver
story. Revisiting any decision below is a contract change, not a build fix.

`lean-rs-sys` is the workspace's raw FFI crate—the curated `extern "C"` view of `lean.h` plus
the pure-Rust mirrors of `lean.h`'s `static inline` refcount helpers, the symbol allowlist,
the header digest, and the link directives. Published as the foundation of the
`lean-rs-sys → lean-toolchain → lean-rs` layering.

This document covers *why*. The *what* lives in the source tree.

## What this design deliberately does not commit to

Pinned upfront so the rationale below reads as boundary-defending, not boundary-extending.

- **Bindgen.** Hand-written allowlist stays. Hand-written is auditable, has no `clang` dep, and is easy to gate on Lean version.
- **Multi-Lean-version support beyond the contiguous supported range.** See [`02-versioning-and-compatibility.md`](02-versioning-and-compatibility.md).
- **Windows.** Per `CI-LINT-BASELINE`, Linux + macOS only.
- **A proc-macro layer.** `REQUIRED_SYMBOLS` is hand-maintained; `tests/symbols_match.rs` codegen is deferred until the allowlist reaches ~80 entries.

## 1. Why `publish = true`

Every comparable peer publishes its `*-sys` crate: `pyo3-ffi`, `libgit2-sys`,
`libsqlite3-sys`, `openssl-sys`, `mlua-sys`, `libz-sys`, `lua-sys`, `wasmtime-c-api`. Reason:
when a user hits a capability the safe layer does not yet wrap, depending on the `*-sys`
crate directly (with full `unsafe` discipline) is dramatically friendlier than forking the
workspace.

The "no raw escape hatch through `lean-rs`" policy that motivated `publish = false` is enforced
inside `lean-rs` by the `pub(crate)` discipline around raw imports—independent of
`lean-rs-sys`'s publication status. The minimum-unsafe public surface (see §2) means even
opt-in users cannot accidentally corrupt the runtime by writing through `*mut lean_object`.

## 2. Why opaque public types + crate-private `LeanObjectRepr`

The naive choice would mirror pyo3-ffi's `pub struct PyObject { pub ob_refcnt, pub ob_type }`:
layout in public, fields direct. For `lean-rs-sys` published the right answer is different.

**Public surface** (`src/types.rs`):

```rust
use core::marker::{PhantomData, PhantomPinned};

#[repr(C)]
pub struct lean_object {
    _data: [u8; 0],
    _marker: PhantomData<(*mut u8, PhantomPinned)>,
}

pub type lean_obj_arg   = *mut lean_object;
pub type b_lean_obj_arg = *mut lean_object;
pub type u_lean_obj_arg = *mut lean_object;
pub type lean_obj_res   = *mut lean_object;
pub type b_lean_obj_res = *mut lean_object;
```

Standard Rust pattern for an FFI-opaque type pre-`extern type` (RFC 1861, unstable). The
`PhantomData<(*mut u8, PhantomPinned)>` makes the type `!Send + !Sync + !Unpin` and prevents
accidental copies; the precise idiom `bindgen --blocklist-type` emits.

**Crate-private repr** (`src/repr.rs`):

```rust
#[repr(C)]
pub(crate) struct LeanObjectRepr {
    pub(crate) m_rc:    i32,   // atomic ops via AtomicI32::from_ptr at call sites
    pub(crate) m_cs_sz: u16,
    pub(crate) m_other: u8,
    pub(crate) m_tag:   u8,
}
```

Plus subclass header reprs (`LeanCtorObjectRepr`, `LeanArrayObjectRepr`,
`LeanStringObjectRepr`, `LeanClosureObjectRepr`, …). Each inline mirror (`lean_inc`,
`lean_dec`, `lean_ptr_tag`, …) casts `*mut lean_object` → `*mut LeanObjectRepr` inside
`unsafe { ... }` blocks with `// SAFETY:` comments naming the invariant.

### Why this is the minimum-unsafe choice for a published crate

1. Downstream code that holds `*mut lean_object` literally cannot write `(*ptr).m_rc = 0`—the public type has zero fields visible. The only path to RC and tag inspection is through `pub unsafe fn` helpers, explicit and correct by construction.
2. Every cast from `*mut lean_object` to `*mut LeanObjectRepr` is a single unsafe operation prefacing a `// SAFETY:` block. The cast does not multiply unsafe scope.
3. Lean's header layout becomes a **crate-private invariant**. Downstream semver decouples from `lean.h`: if Lean reorders header fields in 4.30, the crate updates `LeanObjectRepr` and re-publishes with a bumped Lean-version range; downstream code using `lean_inc`/`lean_dec` is unaffected.
4. Contrast with pyo3-ffi is intentional. CPython documents direct field access (`Py_TYPE(op)` does `(*op).ob_type`). Lean's `lean.h` accesses fields only via inline helpers; no public Lean API names `m_rc` or `m_tag`. The opaque design is honest about Lean's actual contract.

## 3. Why pure-Rust refcount mirrors (not a C shim)

The inline helpers in `lean.h` (`lean_inc`, `lean_dec`, `lean_inc_ref_n`, `lean_dec_ref`,
friends—`lean.h:536–563` of Lean 4.29.1) are `static inline`, not exported symbols. The crate
must either mirror them in Rust or vendor a C shim built via `cc`.

### Why a Rust mirror

- **Faster.** A Rust mirror inlines into `lean-rs`'s `Drop`/`Clone` directly. A C shim defeats inlining across the FFI boundary unless cross-LTO is enabled (fragile, environment-dependent).
- **Fewer build dependencies.** No `cc` build-dep, no `.c` files in the tree, no per-platform compiler shenanigans.

### Why atomic-relaxed everywhere

`core::sync::atomic::AtomicI32` with `Ordering::Relaxed` on all loads, stores, and the MT-path
`fetch_sub`, matching the `memory_order_relaxed` argument the `static inline` C source passes
to `atomic_fetch_sub_explicit`. The cold path is the externally-exported `lean_dec_ref_cold`,
which owns the actual deallocation and its own ordering decisions.

`AtomicI32::from_ptr` (stable since Rust 1.75) is the load-bearing primitive: the mirrors
materialize a `&AtomicI32` from `*mut lean_object` so the actual `load`/`store`/`fetch_sub`
happens on a safe reference. Overflow guards on the single-threaded fast path use
`i32::strict_add` / `i32::strict_sub` (stable since 1.91) so a refcount invariant breach
surfaces as a panic in both debug and release; that drives the workspace `rust-version = "1.91"`.

### How drift is caught

The build script reads the discovered `lean.h`, computes SHA-256, and looks it up in
[`SUPPORTED_TOOLCHAINS`](../../crates/lean-rs-sys/src/supported.rs)—the table of
`(versions, header_digest, missing_symbols)` entries that names the v0.1.0 compatibility
window. A miss fails the build with bounded diagnostics naming the discovered digest and the
full window. The mirrors are byte-identical across every entry (verified empirically; the
table commentary records the layout-stability proof). The mirrors plus the digest check are
the entire trust boundary for the refcount fast path.

## 4. Why split-by-category module layout (~12 files)

For ~80–100 symbols, a single `src/lib.rs` (libgit2-sys / libz-sys style) becomes a 700-LOC
scrolling exercise. A split-by-category layout (openssl-sys / pyo3-ffi style) is the right
answer at this scale:

```
crates/lean-rs-sys/src/
  lib.rs       crate doc, lint allow, re-exports, REQUIRED_SYMBOLS
  consts.rs    LEAN_VERSION, header path, header digest, tag constants
  types.rs     opaque lean_object + ABI typedefs
  refcount.rs  Rust mirrors of lean_inc / lean_dec / friends
  object.rs    ctor / closure alloc, box, unbox, ptr_tag, is_scalar, casts
  scalar.rs    uint*/int*/usize/isize <-> Nat/Int conversions
  string.rs    lean_mk_string, lean_string_cstr, lean_string_size
  array.rs     lean_array_*, lean_sarray_*
  nat_int.rs   bignum dispatchers
  closure.rs   lean_alloc_closure, lean_apply_1..16
  io.rs        lean_io_result_*
  init.rs      lean_initialize, lean_initialize_runtime_module, threads
  external.rs  lean_alloc_external, lean_register_external_class
  repr.rs      pub(crate) struct LeanObjectRepr + subclass repr structs
```

Twelve files at 50–150 LOC each. Easier to navigate on docs.rs, easier to audit per category.
Within each module the convention is "extern decls at top (link-checked), Rust mirrors below";
the per-file doc names the `lean.h` line range the module covers.

## 5. Why C-verbatim naming

Every peer `*-sys` preserves C names verbatim—`PyObject`, `git_repository`, `lua_State`,
`SSL_CTX`. Reasons:

- **Searchability.** Users grep for `lean_inc` and find both `lean.h` and the binding in one shot.
- **No bikeshedding.** 1-to-1 mapping has no judgment calls.
- **Higher-layer renaming is the right place for it.** `lean-rs`'s safe layer wraps these under Rust-style names (`LeanRuntime`, `LeanExpr`, …); `lean-rs-sys` keeps raw names so the boundary is visible.

The crate root enables `#![allow(non_camel_case_types)]` and `#![allow(non_snake_case)]` as a
consequence.

## 6. Why a `REQUIRED_SYMBOLS` allowlist plus a linkage test

The `extern "C"` blocks across `src/*.rs` are the authoritative declarations;
`pub const REQUIRED_SYMBOLS: &[&str]` enumerates them as data for tooling:

- `tests/linkage.rs` takes the address of each entry; if any symbol is missing in `libleanshared`, the binary fails to link and the test fails.
- `lean-toolchain`'s `required_symbols()` returns `lean_rs_sys::REQUIRED_SYMBOLS` directly—the allowlist lives once.
- Future tooling (version-compatibility checks, documentation, the charter) can iterate the list without parsing source.

Hand-maintaining ~75 entries is acceptable; churn is low. A future `tests/symbols_match.rs`
could grep the `extern "C"` blocks and assert equivalence if drift becomes a real problem.

## 7. Why a single-file `build.rs` and `sha2` as the only build-dep

The script's job is small:

1. Discover Lean (env vars → `lean --print-prefix` → fixture Lake env, in order, with bounded diagnostics on each miss).
2. Read `<prefix>/include/lean/lean.h` and compute SHA-256.
3. Look up the digest in [`SUPPORTED_TOOLCHAINS`](../../crates/lean-rs-sys/src/supported.rs). On a hit, record the matched resolved version; on a miss, fail the build with the discovered digest and the supported window.
4. Emit `cargo:rustc-env=LEAN_{VERSION,RESOLVED_VERSION,HEADER_PATH,HEADER_DIGEST}=…` plus `cargo:rustc-cfg=lean_v_X_Y_Z` for the matched entry.
5. Emit `cargo:rustc-link-*` directives based on features (`static` vs `dynamic`, `mimalloc`). The default is `dynamic` (see §10).
6. Emit `cargo:rerun-if-{env-changed,changed}=…`.

At ~150 LOC, a single `build.rs` is right. Splitting into a `build/` directory (openssl-sys
style) is overkill. `sha2` is the sole runtime dependency; no `bindgen`, no `cc`, no
`pkg-config`.

## 8. Minimum-unsafe discipline

1. `#![allow(unsafe_code)]` at the crate root only; no per-file silent allows.
2. Every `pub unsafe fn` carries a `# Safety` doc section naming pre-conditions (typically a Lean ABI invariant or a layout assumption pinned by `LEAN_HEADER_DIGEST`).
3. Every `unsafe { ... }` block carries a `// SAFETY:` comment naming the invariant.
4. No public `pub` fields on any FFI type. Layout access is exclusively through `pub unsafe fn` helpers that cast `*mut lean_object` → `*mut LeanObjectRepr` internally.
5. `NonNull<lean_object>` at API edges where Lean's ABI guarantees non-null; raw `*mut lean_object` only where the C ABI permits null.
6. `AtomicI32::from_ptr` (Rust 1.75+) inside the refcount mirrors so the actual `load`/`store`/`fetch_sub` happens on a safe `&AtomicI32`.
7. No `transmute`; all pointer reshaping is `.cast::<T>()` or `&raw mut` with `// SAFETY:` justification.
8. Refcount and allocation-size arithmetic uses `i32::strict_add` / `usize::strict_add` / `strict_mul` (Rust 1.91+)—overflow panics in debug *and* release, surfacing invariant breaches instead of silently producing a wrong size or wrapped refcount.

## 9. As-built deviations

The sections above describe the approved design. Implementation surfaced these deviations:

- **Default features are `["mimalloc", "dynamic"]`, not `["mimalloc", "static"]`.** The prompt's static link set (`Lean`, `Init`, `leanrt`, `leancpp`, `Lake`) does not actually link a Lean stdlib symbol-using program without at least `libStd.a` and a specific archive order. The default switched to dynamic so the test binary links against `libleanshared` out of the box. The `static` feature is preserved for embedders who explicitly want it.
- **The `mimalloc` feature is a no-op marker.** Lean 4.29.1's mimalloc is statically linked into `libleanrt.a` / `libleanshared`; no separate `libmimalloc` exists to link against. The feature stays as a marker downstream tooling can read and a hook for future toolchains that ship mimalloc separately.
- **`LeanObjectRepr::m_rc` is `i32`, not `AtomicI32`.** `AtomicI32::from_ptr` takes `*mut i32`; storing as plain `i32` makes the cast ergonomic and keeps the layout byte-exact. Atomic semantics happen at the call site, not in the struct.
- **`Init` symbols live in `init.rs` but not in `lean.h`.** `lean_initialize`, `lean_initialize_runtime_module`, `lean_initialize_thread`, `lean_finalize_thread`, `lean_setup_args` are exported by `libleanshared` but not declared in `lean.h`. They appear in `init.rs` as `extern "C"` declarations with the standard runtime signatures. The `LEAN_HEADER_DIGEST` check does not gate them (it guards layout, not runtime entry points); `REQUIRED_SYMBOLS` plus `tests/linkage.rs` covers them instead.
- **`REQUIRED_SYMBOLS` has ~75 entries** (vs the ~50–80 estimate). Items the prompt prefigured as externs are actually `static inline` in 4.29.1 (`lean_alloc_ctor_memory`, `lean_alloc_closure`, `lean_alloc_array`). Those are mirrored in Rust and dropped from the allowlist; `lean_alloc_object` / `lean_free_object` (the real externs) are listed instead.
- **Layout assertions for `pub(crate) LeanObjectRepr` and friends live in `#[cfg(test)] mod tests` inside `src/repr.rs`**, not `tests/layout.rs`. Integration tests cannot see `pub(crate)` items; the unit-test module keeps internals internal without leaking a `#[doc(hidden)] pub mod __test` accessor.
- **The digest-mismatch fixture test is documented, not automated.** Verification step 6 in the prompt called for a build-time test that flips a `lean.h` byte and asserts failure. Automating it would require either an in-tree fixture sysroot or a noisy subprocess test; the procedure is documented for manual exercise in the crate's `README.md`.
- **MSRV is 1.91**, originally planned at `1.85` for `AtomicI32::from_ptr` (1.75). Bumped to use `strict_add` / `strict_sub` / `strict_mul` (1.91) so refcount or size overflow panics in debug and release instead of silent wrap.
- **Lint discipline.** Doc lints (`doc_markdown`, `missing_safety_doc`, `undocumented_unsafe_blocks`, `too_long_first_doc_paragraph`, `missing_inline_in_public_items`, `missing_panics_doc`) are never silenced; every violation is fixed. Narrow module-level allows persist for `clippy::inline_always` (FFI mirror modules where always-inline is the design), `clippy::struct_field_names` (`repr.rs`—`m_*` mirrors C), and `clippy::cast_possible_*` / `cast_sign_loss` (`scalar.rs`—C ABI mandates the narrowing shape).

## Cross-references

- Charter: [`00-charter.md`](00-charter.md)—*Adopted shape*.
- Safety model: [`01-safety-model.md`](01-safety-model.md)—*Unsafe boundary*.
- Version compatibility: [`02-versioning-and-compatibility.md`](02-versioning-and-compatibility.md)—*Header digest*, *Bumping the Lean version*.
- L1 safe surface: the `lean-rs` crate (typed FFI primitive). Crate docs at <https://docs.rs/lean-rs>.
- L2 opinionated stack: [`04-host-stack.md`](04-host-stack.md)—the `lean-rs-host` crate, what gets built on top of the L1 safe surface.
