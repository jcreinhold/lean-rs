# `lean-rs-sys` design rationale

This document is the per-decision rationale behind the `lean-rs-sys` crate's
shape. Prompt 04 (`prompts/lean-rs/04-raw-sys-bindings.md`) implements it; the
charter and the safety-model docs depend on it; the version-compatibility doc
references it for the semver story. If a future change wants to revisit any
of the decisions below, treat that as a contract change (`RD-…`), not a build
fix.

`lean-rs-sys` is the workspace's raw FFI crate — the curated `extern "C"`
view of `lean.h` plus the pure-Rust mirrors of `lean.h`'s `static inline`
refcount helpers, the symbol allowlist, the header digest, and the
link directives. It is published as the foundation of the
`lean-rs-sys → lean-toolchain → lean-rs` layering.

This document focuses on *why* the crate has the shape it has. The *what*
lives in prompt 04 (concrete file layout, Cargo.toml, code sketches) and in
the contract entries in `prompts/lean-rs/00-current-state.md` (the live
surface as it lands).

## 1. Why `publish = true`

`RD-2026-05-17-003` originally set `publish = false` to encode a "no raw
escape hatch" policy. `RD-2026-05-17-005` reversed that. Every comparable
peer publishes its `*-sys` crate: `pyo3-ffi`, `libgit2-sys`,
`libsqlite3-sys`, `openssl-sys`, `mlua-sys`, `libz-sys`, `lua-sys`,
`wasmtime-c-api`. The reason is the same in each case: when a user hits a
capability the safe layer does not yet wrap, depending on the `*-sys` crate
directly (with full `unsafe` discipline) is dramatically friendlier than
forking the workspace.

The "no raw escape hatch through `lean-rs`" policy that motivated
`publish = false` is enforced inside `lean-rs` by the `pub(crate)`
discipline around raw imports, independent of `lean-rs-sys`'s
publication status. Publishing `lean-rs-sys` does not weaken the
safe-front-door story; it just makes the unsafe raw API reachable to those
who explicitly opt in with all the type-system pain that entails.

The minimum-unsafe public surface (see §2) means even opt-in users cannot
accidentally corrupt the runtime by writing through `*mut lean_object`.

## 2. Why opaque public types + crate-private `LeanObjectRepr`

The naive choice would mirror pyo3-ffi's `pub struct PyObject { pub ob_refcnt,
pub ob_type }`: layout in public, downstream code accesses fields directly.
For `lean-rs-sys` published the right answer is different.

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

This is the standard Rust pattern for an FFI-opaque type pre-`extern type`
(RFC 1861, unstable). The `PhantomData<(*mut u8, PhantomPinned)>` makes the
type `!Send + !Sync + !Unpin` and prevents accidental copies; the precise
idiom that `bindgen --blocklist-type` emits.

**Crate-private repr** (`src/repr.rs`):

```rust
#[repr(C)]
pub(crate) struct LeanObjectRepr {
    pub(crate) m_rc:    core::sync::atomic::AtomicI32,
    pub(crate) m_cs_sz: u16,
    pub(crate) m_other: u8,
    pub(crate) m_tag:   u8,
}
```

Plus subclass header reprs (`LeanCtorObjectRepr`, `LeanArrayObjectRepr`,
`LeanStringObjectRepr`, `LeanClosureObjectRepr`, …) following the same
pattern. Each inline mirror (`lean_inc`, `lean_dec`, `lean_ptr_tag`, …)
casts `*mut lean_object` → `*mut LeanObjectRepr` inside `unsafe { ... }`
blocks with `// SAFETY:` comments naming the invariant.

### Why this is the minimum-unsafe choice for a published crate

1. Downstream code that holds `*mut lean_object` literally cannot write
   `(*ptr).m_rc = 0` because the public type has zero fields visible. The
   only path to RC and tag inspection is through `pub unsafe fn` helpers —
   explicit and correct by construction.
2. Every cast from `*mut lean_object` to `*mut LeanObjectRepr` is a
   *single* unsafe operation inside the crate, prefacing a `// SAFETY:`
   block. The cast does not multiply unsafe scope — it is the same unsafe
   a `pub` field would have, contained at the boundary.
3. Lean's header layout becomes a **crate-private invariant**. Downstream
   semver decouples from `lean.h` changes: if Lean reorders header fields
   in 4.30, the crate updates `LeanObjectRepr` and re-publishes with a
   bumped Lean-version range; downstream code using `lean_inc`/`lean_dec`
   is unaffected.
4. The contrast with pyo3-ffi's pub-fields layout is intentional: CPython's
   API documents direct field access (`Py_TYPE(op)` is a macro that does
   `(*op).ob_type`). Lean's `lean.h` accesses fields only via inline
   helpers; there is no public Lean API that names `m_rc` or `m_tag`
   directly. The opaque design is honest about Lean's actual contract.

## 3. Why pure-Rust refcount mirrors (not a C shim)

The inline helpers in `lean.h` (`lean_inc`, `lean_dec`, `lean_inc_ref_n`,
`lean_dec_ref`, friends — lines 536–563 of Lean 4.29.1) are `static
inline` — not exported symbols. The crate must either mirror them in Rust
or vendor a C shim built via the `cc` crate.

The Rust mirror wins on three axes:

- **Faster.** A Rust mirror inlines into `lean-rs`'s `Drop`/`Clone`
  directly. A C shim defeats inlining across the FFI boundary unless
  cross-LTO is enabled (fragile, environment-dependent).
- **Fewer build dependencies.** No `cc` build-dep, no `.c` files in the
  tree, no per-platform compiler shenanigans.
- **Drift is structurally caught.** The build script reads the discovered
  `lean.h`, computes SHA-256, and compares against `EXPECTED_HEADER_DIGEST`
  (hard-coded in `lib.rs`, the digest the refcount mirrors were authored
  against). A mismatch fails the build with bounded diagnostics naming
  both digests and the discovered header path.

The mirrors use `core::sync::atomic::AtomicI32` with `Ordering::Relaxed` on
loads/stores and `Ordering::Release` on the cold-path `fetch_sub` that
matches `lean_dec_ref`'s drop semantics. The `static inline` C → Rust
translation is literal; the mirrors plus the digest check are the entire
trust boundary for the refcount fast path.

Rust 1.75's `AtomicI32::from_ptr` is the load-bearing primitive: the
refcount mirrors create a `&AtomicI32` from `*mut lean_object` so the actual
`load`/`store`/`fetch_sub` happens on a safe reference, not a raw
atomic-typed pointer. The workspace `rust-version = "1.85"` covers this
comfortably.

## 4. Why split-by-category module layout (~12 files)

For ~80–100 symbols, a single `src/lib.rs` (libgit2-sys / libz-sys style)
becomes a 700-LOC scrolling exercise. A split-by-category layout
(openssl-sys / pyo3-ffi style) is the right answer at this scale:

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

Twelve files at 50–150 LOC each. Easier to navigate on docs.rs, easier to
audit per category. Within each module the convention is "extern decls at
top (link-checked), Rust mirrors below"; the per-file doc comment names
the `lean.h` line range the module covers.

## 5. Why C-verbatim naming

Every peer `*-sys` preserves C names verbatim — `PyObject`, `git_repository`,
`lua_State`, `SSL_CTX`. Reasons:

- **Searchability.** Users grep for `lean_inc` and find both lean.h and
  the binding in one shot.
- **No bikeshedding.** 1-to-1 mapping has no judgment calls.
- **Higher-layer renaming is the right place for it.** `lean-rs`'s safe
  layer wraps these under Rust-style names (`LeanRuntime`, `LeanExpr`,
  …); `lean-rs-sys` keeps the raw names so the boundary is visible.

The crate root enables `#![allow(non_camel_case_types)]` and
`#![allow(non_snake_case)]` as a consequence.

## 6. Why a `REQUIRED_SYMBOLS` allowlist plus a linkage test

The `extern "C"` blocks across `src/*.rs` are the authoritative
declarations; `pub const REQUIRED_SYMBOLS: &[&str]` enumerates them as
data for tooling:

- `tests/linkage.rs` takes the address of each entry; if any symbol is
  missing in `libleanshared`, the binary fails to link and the test fails.
- `lean-toolchain`'s `required_symbols()` returns `lean_rs_sys::REQUIRED_SYMBOLS`
  directly — the allowlist lives once.
- Future tooling (version-compatibility checks, documentation, the
  charter) can iterate the list without parsing source.

Hand-maintaining ~50–80 entries is acceptable; the churn rate is low. A
future `tests/symbols_match.rs` could grep the `extern "C"` blocks and
assert equivalence with the const if drift becomes a real problem.

## 7. Why a single-file `build.rs` and `sha2` as the only build-dep

The build script's job is small:

1. Discover Lean (env vars → `lean --print-prefix` → fixture Lake env, in
   order, with bounded diagnostics on each miss).
2. Read `<prefix>/include/lean/lean.h` and compute SHA-256.
3. Emit `cargo:rustc-env=LEAN_{VERSION,HEADER_PATH,HEADER_DIGEST}=…`.
4. Assert the digest matches `EXPECTED_HEADER_DIGEST` (compile-time
   pinned in `lib.rs`).
5. Emit `cargo:rustc-link-*` directives based on features (`static` vs
   `dynamic`, `mimalloc`).
6. Emit `cargo:rerun-if-{env-changed,changed}=…`.

At ~150 LOC, a single `build.rs` is right — splitting into a `build/`
directory (openssl-sys style) is overkill at this scale. `sha2` is the
sole runtime dependency; no `bindgen`, no `cc`, no `pkg-config`.

## 8. Minimum-unsafe discipline

Per the user's explicit ask:

1. `#![allow(unsafe_code)]` at the crate root only; no per-file silent
   allows.
2. Every `pub unsafe fn` carries a `# Safety` doc section naming
   pre-conditions (typically a Lean ABI invariant or a layout assumption
   pinned by `LEAN_HEADER_DIGEST`).
3. Every `unsafe { ... }` block carries a `// SAFETY:` comment naming the
   invariant.
4. No public `pub` fields on any FFI type. Layout access is exclusively
   through `pub unsafe fn` helpers that cast `*mut lean_object` →
   `*mut LeanObjectRepr` internally.
5. Use `NonNull<lean_object>` at API edges where Lean's ABI guarantees
   non-null; raw `*mut lean_object` only where the C ABI permits null.
6. `AtomicI32::from_ptr` (stable since Rust 1.75) inside the refcount
   mirrors so the actual `load`/`store`/`fetch_sub` happens on a safe
   `&AtomicI32`.
7. No `transmute`; all pointer reshaping is `as`-cast with `// SAFETY:`
   justification.

## 9. What this design deliberately does *not* commit to

- **Bindgen.** The hand-written-allowlist commitment from the charter
  stays. Hand-written is auditable, has no `clang` dep, and is easy to gate
  on Lean version.
- **Multi-Lean-version support beyond the contiguous supported range.**
  See `02-versioning-and-compatibility.md`.
- **Windows.** Per `CI-LINT-BASELINE`, Linux + macOS only.
- **A proc-macro layer.** `REQUIRED_SYMBOLS` is hand-maintained;
  `tests/symbols_match.rs` codegen is deferred until the allowlist
  reaches ~80 entries.

## Cross-references

- Implementation prompt: `prompts/lean-rs/04-raw-sys-bindings.md`.
- Charter: `docs/architecture/00-charter.md` (§"Smallest public interface",
  §"Adopted").
- Safety model: `docs/architecture/01-safety-model.md` (§"Unsafe boundary
  thesis").
- Version compatibility: `docs/architecture/02-versioning-and-compatibility.md`
  (§"Header digest", §"Bumping the Lean version: process").
- Host-API surface: `docs/architecture/03-host-api.md` — what gets built
  on top of this crate.
- Live contract state: `prompts/lean-rs/00-current-state.md` — `RAW-SYS`,
  `WORKSPACE-LAYERS`, `VERSION-COMPATIBILITY`, `SAFETY-MODEL`.
- Replanning deltas: `RD-2026-05-17-005` (publication + opaque types +
  pure-Rust mirrors), `RD-2026-05-17-003` (in-tree raw FFI),
  `RD-2026-05-17-004` (lean-rs internal compression).
