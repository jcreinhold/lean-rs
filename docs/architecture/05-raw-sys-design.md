# `lean-rs-sys` design rationale

This document is the per-decision rationale behind the `lean-rs-sys` crate's shape. Prompt 04
(`prompts/lean-rs/04-raw-sys-bindings.md`) implements it; the charter and the safety-model docs depend on it; the
version-compatibility doc references it for the semver story. If a future change wants to revisit any of the decisions
below, treat that as a contract change (`RD-…`), not a build fix.

`lean-rs-sys` is the workspace's raw FFI crate — the curated `extern "C"` view of `lean.h` plus the pure-Rust mirrors of
`lean.h`'s `static inline` refcount helpers, the symbol allowlist, the header digest, and the link directives. It is
published as the foundation of the `lean-rs-sys → lean-toolchain → lean-rs` layering.

This document focuses on _why_ the crate has the shape it has. The _what_ lives in prompt 04 (concrete file layout,
Cargo.toml, code sketches) and in the contract entries in `prompts/lean-rs/00-current-state.md` (the live surface as it
lands).

## 1. Why `publish = true`

`RD-2026-05-17-003` originally set `publish = false` to encode a "no raw escape hatch" policy. `RD-2026-05-17-005`
reversed that. Every comparable peer publishes its `*-sys` crate: `pyo3-ffi`, `libgit2-sys`, `libsqlite3-sys`,
`openssl-sys`, `mlua-sys`, `libz-sys`, `lua-sys`, `wasmtime-c-api`. The reason is the same in each case: when a user
hits a capability the safe layer does not yet wrap, depending on the `*-sys` crate directly (with full `unsafe`
discipline) is dramatically friendlier than forking the workspace.

The "no raw escape hatch through `lean-rs`" policy that motivated `publish = false` is enforced inside `lean-rs` by the
`pub(crate)` discipline around raw imports, independent of `lean-rs-sys`'s publication status. Publishing `lean-rs-sys`
does not weaken the safe-front-door story; it just makes the unsafe raw API reachable to those who explicitly opt in
with all the type-system pain that entails.

The minimum-unsafe public surface (see §2) means even opt-in users cannot accidentally corrupt the runtime by writing
through `*mut lean_object`.

## 2. Why opaque public types + crate-private `LeanObjectRepr`

The naive choice would mirror pyo3-ffi's `pub struct PyObject { pub ob_refcnt, pub ob_type }`: layout in public,
downstream code accesses fields directly. For `lean-rs-sys` published the right answer is different.

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

This is the standard Rust pattern for an FFI-opaque type pre-`extern type` (RFC 1861, unstable). The
`PhantomData<(*mut u8, PhantomPinned)>` makes the type `!Send + !Sync + !Unpin` and prevents accidental copies; the
precise idiom that `bindgen --blocklist-type` emits.

**Crate-private repr** (`src/repr.rs`):

```rust
#[repr(C)]
pub(crate) struct LeanObjectRepr {
    pub(crate) m_rc:    i32,   // see "as built" note below — stored as i32; atomic ops via `AtomicI32::from_ptr` at call sites
    pub(crate) m_cs_sz: u16,
    pub(crate) m_other: u8,
    pub(crate) m_tag:   u8,
}
```

Plus subclass header reprs (`LeanCtorObjectRepr`, `LeanArrayObjectRepr`, `LeanStringObjectRepr`,
`LeanClosureObjectRepr`, …) following the same pattern. Each inline mirror (`lean_inc`, `lean_dec`, `lean_ptr_tag`, …)
casts `*mut lean_object` → `*mut LeanObjectRepr` inside `unsafe { ... }` blocks with `// SAFETY:` comments naming the
invariant.

### Why this is the minimum-unsafe choice for a published crate

1. Downstream code that holds `*mut lean_object` literally cannot write `(*ptr).m_rc = 0` because the public type has
    zero fields visible. The only path to RC and tag inspection is through `pub unsafe fn` helpers — explicit and
    correct by construction.
1. Every cast from `*mut lean_object` to `*mut LeanObjectRepr` is a _single_ unsafe operation inside the crate,
    prefacing a `// SAFETY:` block. The cast does not multiply unsafe scope — it is the same unsafe a `pub` field would
    have, contained at the boundary.
1. Lean's header layout becomes a **crate-private invariant**. Downstream semver decouples from `lean.h` changes: if
    Lean reorders header fields in 4.30, the crate updates `LeanObjectRepr` and re-publishes with a bumped Lean-version
    range; downstream code using `lean_inc`/`lean_dec` is unaffected.
1. The contrast with pyo3-ffi's pub-fields layout is intentional: CPython's API documents direct field access
    (`Py_TYPE(op)` is a macro that does `(*op).ob_type`). Lean's `lean.h` accesses fields only via inline helpers;
    there is no public Lean API that names `m_rc` or `m_tag` directly. The opaque design is honest about Lean's actual
    contract.

## 3. Why pure-Rust refcount mirrors (not a C shim)

The inline helpers in `lean.h` (`lean_inc`, `lean_dec`, `lean_inc_ref_n`, `lean_dec_ref`, friends — lines 536–563 of
Lean 4.29.1) are `static inline` — not exported symbols. The crate must either mirror them in Rust or vendor a C shim
built via the `cc` crate.

The Rust mirror wins on three axes:

- **Faster.** A Rust mirror inlines into `lean-rs`'s `Drop`/`Clone` directly. A C shim defeats inlining across the FFI
    boundary unless cross-LTO is enabled (fragile, environment-dependent).
- **Fewer build dependencies.** No `cc` build-dep, no `.c` files in the tree, no per-platform compiler shenanigans.
- **Drift is structurally caught.** The build script reads the discovered `lean.h`, computes SHA-256, and looks it up
    in [`SUPPORTED_TOOLCHAINS`](../../crates/lean-rs-sys/src/supported.rs) — the table of `(versions, header_digest,
    missing_symbols)` entries that names the v0.1.0 compatibility window. A miss fails the build with bounded
    diagnostics naming the discovered digest and the full window. The mirrors are byte-identical across every entry in
    the window (verified empirically; the table commentary records the layout-stability proof).

The mirrors use `core::sync::atomic::AtomicI32` with `Ordering::Relaxed` on all loads/stores and on the MT-path
`fetch_sub`, matching the `memory_order_relaxed` argument the `static inline` C source passes to
`atomic_fetch_sub_explicit`. The cold path is the externally-exported `lean_dec_ref_cold`, which owns the actual
deallocation. The mirrors plus the digest check are the entire trust boundary for the refcount fast path.

`AtomicI32::from_ptr` (stable since Rust 1.75) is the load-bearing primitive: the refcount mirrors materialize a
`&AtomicI32` from a `*mut lean_object` so the actual `load`/`store`/`fetch_sub` happens on a safe reference. Overflow
guards on the single-threaded fast path use `i32::strict_add` / `i32::strict_sub` (stable since Rust 1.91) so a refcount
invariant breach surfaces as a panic in both debug and release; that drives the workspace `rust-version = "1.91"`.

## 4. Why split-by-category module layout (~12 files)

For ~80–100 symbols, a single `src/lib.rs` (libgit2-sys / libz-sys style) becomes a 700-LOC scrolling exercise. A
split-by-category layout (openssl-sys / pyo3-ffi style) is the right answer at this scale:

```
crates/lean-rs-sys/src/
  lib.rs       — crate doc, lint allow, re-exports, REQUIRED_SYMBOLS
  consts.rs    — LEAN_VERSION, header path, header digest, tag constants
  types.rs     — opaque lean_object + ABI typedefs
  refcount.rs  — Rust mirrors of lean_inc / lean_dec / friends
  object.rs    — ctor / closure alloc, box, unbox, ptr_tag, is_scalar, casts
  scalar.rs    — uint*/int*/usize/isize <-> Nat/Int conversions
  string.rs    — lean_mk_string, lean_string_cstr, lean_string_size
  array.rs     — lean_array_*, lean_sarray_*
  nat_int.rs   — bignum dispatchers
  closure.rs   — lean_alloc_closure, lean_apply_1..16
  io.rs        — lean_io_result_*
  init.rs      — lean_initialize, lean_initialize_runtime_module, threads
  external.rs  — lean_alloc_external, lean_register_external_class
  repr.rs      — pub(crate) struct LeanObjectRepr + subclass repr structs
```

Twelve files at 50–150 LOC each. Easier to navigate on docs.rs, easier to audit per category. Within each module the
convention is "extern decls at top (link-checked), Rust mirrors below"; the per-file doc comment names the `lean.h` line
range the module covers.

## 5. Why C-verbatim naming

Every peer `*-sys` preserves C names verbatim — `PyObject`, `git_repository`, `lua_State`, `SSL_CTX`. Reasons:

- **Searchability.** Users grep for `lean_inc` and find both lean.h and the binding in one shot.
- **No bikeshedding.** 1-to-1 mapping has no judgment calls.
- **Higher-layer renaming is the right place for it.** `lean-rs`'s safe layer wraps these under Rust-style names
    (`LeanRuntime`, `LeanExpr`, …); `lean-rs-sys` keeps the raw names so the boundary is visible.

The crate root enables `#![allow(non_camel_case_types)]` and `#![allow(non_snake_case)]` as a consequence.

## 6. Why a `REQUIRED_SYMBOLS` allowlist plus a linkage test

The `extern "C"` blocks across `src/*.rs` are the authoritative declarations; `pub const REQUIRED_SYMBOLS: &[&str]`
enumerates them as data for tooling:

- `tests/linkage.rs` takes the address of each entry; if any symbol is missing in `libleanshared`, the binary fails to
    link and the test fails.
- `lean-toolchain`'s `required_symbols()` returns `lean_rs_sys::REQUIRED_SYMBOLS` directly — the allowlist lives once.
- Future tooling (version-compatibility checks, documentation, the charter) can iterate the list without parsing source.

Hand-maintaining ~50–80 entries is acceptable; the churn rate is low. A future `tests/symbols_match.rs` could grep the
`extern "C"` blocks and assert equivalence with the const if drift becomes a real problem.

## 7. Why a single-file `build.rs` and `sha2` as the only build-dep

The build script's job is small:

1. Discover Lean (env vars → `lean --print-prefix` → fixture Lake env, in order, with bounded diagnostics on each miss).
1. Read `<prefix>/include/lean/lean.h` and compute SHA-256.
1. Look up the digest in [`SUPPORTED_TOOLCHAINS`](../../crates/lean-rs-sys/src/supported.rs). On a hit, record the
    matched entry's resolved version; on a miss, fail the build with the discovered digest and the supported window.
1. Emit `cargo:rustc-env=LEAN_{VERSION,RESOLVED_VERSION,HEADER_PATH,HEADER_DIGEST}=…` plus
    `cargo:rustc-cfg=lean_v_X_Y_Z` for the matched entry.
1. Emit `cargo:rustc-link-*` directives based on features (`static` vs `dynamic`, `mimalloc`). The default is `dynamic`
    — see the "as built" note below for why.
1. Emit `cargo:rerun-if-{env-changed,changed}=…`.

At ~150 LOC, a single `build.rs` is right — splitting into a `build/` directory (openssl-sys style) is overkill at this
scale. `sha2` is the sole runtime dependency; no `bindgen`, no `cc`, no `pkg-config`.

## 8. Minimum-unsafe discipline

Per the user's explicit ask:

1. `#![allow(unsafe_code)]` at the crate root only; no per-file silent allows.
1. Every `pub unsafe fn` carries a `# Safety` doc section naming pre-conditions (typically a Lean ABI invariant or a
    layout assumption pinned by `LEAN_HEADER_DIGEST`).
1. Every `unsafe { ... }` block carries a `// SAFETY:` comment naming the invariant.
1. No public `pub` fields on any FFI type. Layout access is exclusively through `pub unsafe fn` helpers that cast
    `*mut lean_object` → `*mut LeanObjectRepr` internally.
1. Use `NonNull<lean_object>` at API edges where Lean's ABI guarantees non-null; raw `*mut lean_object` only where the C
    ABI permits null.
1. `AtomicI32::from_ptr` (stable since Rust 1.75) inside the refcount mirrors so the actual `load`/`store`/`fetch_sub`
    happens on a safe `&AtomicI32`.
1. No `transmute`; all pointer reshaping is `.cast::<T>()` or `&raw mut` with `// SAFETY:` justification.
1. Refcount and allocation-size arithmetic uses `i32::strict_add` / `usize::strict_add` / `strict_mul` (Rust 1.91+) —
    overflow panics in debug _and_ release, surfacing invariant breaches instead of silently producing a wrong size or
    wrapped refcount.

## 9. What this design deliberately does _not_ commit to

- **Bindgen.** The hand-written-allowlist commitment from the charter stays. Hand-written is auditable, has no `clang`
    dep, and is easy to gate on Lean version.
- **Multi-Lean-version support beyond the contiguous supported range.** See `02-versioning-and-compatibility.md`.
- **Windows.** Per `CI-LINT-BASELINE`, Linux + macOS only.
- **A proc-macro layer.** `REQUIRED_SYMBOLS` is hand-maintained; `tests/symbols_match.rs` codegen is deferred until the
    allowlist reaches ~80 entries.

## 10. As-built notes (deviations from the design above)

The rationale sections above describe the design as it was approved. Implementation surfaced a handful of small
deviations worth recording here so the doc matches what shipped:

- **Default features are `["mimalloc", "dynamic"]`, not `["mimalloc", "static"]`.** The prompt's static link set
    (`Lean`, `Init`, `leanrt`, `leancpp`, `Lake`) does not actually link a Lean stdlib symbol-using program without at
    least `libStd.a` and a specific archive order. Rather than expand the static set, the default switched to dynamic so
    the test binary links against `libleanshared` out of the box. The `static` feature is preserved for embedders who
    explicitly want it and will extend the link list to suit their target.
- **The `mimalloc` feature is a no-op marker.** Lean 4.29.1's mimalloc is statically linked into `libleanrt.a` /
    `libleanshared`; there is no separate `libmimalloc` in the toolchain to link against. The feature stays in the
    manifest as a marker downstream tooling can read and as a hook for future toolchains that ship mimalloc separately.
- **`LeanObjectRepr::m_rc` is `i32`, not `AtomicI32`.** `AtomicI32::from_ptr` takes `*mut i32`, so storing the field as
    a plain `i32` makes the cast ergonomic and keeps the layout byte-exact with `lean.h`. Atomic semantics happen at the
    call site, not in the struct definition.
- **`Ordering::Relaxed` everywhere; the cold path is `lean_dec_ref_cold`.** The single-threaded fast path is a plain
    `Relaxed` load/store and the multi-threaded path is `fetch_sub(_, Relaxed)`, matching the C source's
    `memory_order_relaxed`. The "Release on cold-path fetch_sub" wording in §3 was incorrect; the cold path is the
    `LEAN_EXPORT`'d `lean_dec_ref_cold`, which owns its own ordering decisions.
- **MSRV bumped to 1.91.** Originally `1.85` to clear `AtomicI32::from_ptr` (1.75). Bumped to `1.91` to use `strict_add`
    / `strict_sub` / `strict_mul` on the overflow guards — a refcount overflow or size-arithmetic overflow panics in
    both debug and release, instead of producing a silent wrap or under-allocation.
- **Init symbols live in `init.rs` but not in `lean.h`.** `lean_initialize`, `lean_initialize_runtime_module`,
    `lean_initialize_thread`, `lean_finalize_thread`, and `lean_setup_args` are exported by `libleanshared` but are not
    declared in `lean.h`. They appear in `init.rs` as `extern "C"` declarations with the standard runtime signatures.
    The `LEAN_HEADER_DIGEST` check does _not_ gate them (it guards layout, not runtime entry points); they are protected
    by `REQUIRED_SYMBOLS` plus `tests/linkage.rs` instead.
- **`REQUIRED_SYMBOLS` has ~75 entries**, not the ~50–80 estimate from §6, and a few items the prompt prefigured as
    externs are actually `static inline` in 4.29.1 (`lean_alloc_ctor_memory`, `lean_alloc_closure`, `lean_alloc_array`).
    Those are mirrored in Rust and dropped from the allowlist; `lean_alloc_object` / `lean_free_object` (the real
    externs) are listed instead.
- **Layout assertions for `pub(crate) LeanObjectRepr` and friends live in `#[cfg(test)] mod tests` inside
    `src/repr.rs`**, not in `tests/layout.rs`. Integration tests cannot see `pub(crate)` items; the unit-test module
    inside `repr.rs` keeps the internal types internal without leaking a `#[doc(hidden)] pub mod __test` accessor.
- **The digest-mismatch fixture test is documented, not automated.** Verification step 6 in the prompt called for a
    build-time test that flips a `lean.h` byte and asserts the build fails. Automating it would require either a fixture
    sysroot in-tree or a noisy subprocess test; the procedure is documented for manual exercise in the crate's
    `README.md` instead.
- **Lint discipline as shipped.** Doc-related lints (`doc_markdown`, `missing_safety_doc`, `undocumented_unsafe_blocks`,
    `too_long_first_doc_paragraph`, `missing_inline_in_public_items`, `missing_panics_doc`) are never silenced; every
    violation is fixed at the source. Narrow module-level allows for `clippy::inline_always` (on FFI mirror modules
    where always-inline is the design), `clippy::struct_field_names` (`repr.rs` only — `m_*` mirrors C), and
    `clippy::cast_possible_*` / `cast_sign_loss` (`scalar.rs` only — C ABI mandates the narrowing shape) are the only
    persistent exceptions.

## Cross-references

- Implementation prompt: `prompts/lean-rs/04-raw-sys-bindings.md`.
- Charter: `docs/architecture/00-charter.md` (§"Smallest public interface", §"Adopted").
- Safety model: `docs/architecture/01-safety-model.md` (§"Unsafe boundary thesis").
- Version compatibility: `docs/architecture/02-versioning-and-compatibility.md` (§"Header digest", §"Bumping the Lean
    version: process").
- L1 safe surface: the `lean-rs` crate (typed FFI primitive — runtime, library, module, typed `@[export]` dispatch,
    handles, structured error). Crate docs at `https://docs.rs/lean-rs`.
- L2 opinionated stack: `docs/architecture/04-host-stack.md` — the `lean-rs-host` crate, what gets built on top of the
    L1 safe surface (`LeanHost` / `LeanCapabilities` / `LeanSession` plus elaboration / evidence / meta / pool surfaces
    and the 13 + 3 `lean_rs_host_*` Lean shim contract).
- Live contract state: `prompts/lean-rs/00-current-state.md` — `RAW-SYS`, `WORKSPACE-LAYERS`, `VERSION-COMPATIBILITY`,
    `SAFETY-MODEL`.
- Replanning deltas: `RD-2026-05-17-005` (publication + opaque types + pure-Rust mirrors), `RD-2026-05-17-003` (in-tree
    raw FFI), `RD-2026-05-17-004` (lean-rs internal compression), `RD-2026-05-18-001` (L1/L2 split into `lean-rs` and
    `lean-rs-host`).
