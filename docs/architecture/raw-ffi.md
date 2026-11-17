# Raw FFI Design

`lean-rs-sys` publishes the curated Lean C ABI declarations, Rust mirrors of required `lean.h` inlines, opaque FFI
types, and runtime link directives. `lean-rs-abi` supplies static symbol, toolchain-window, and layout metadata without
linking or probing Lean. `lean-toolchain` owns discovery and build helpers. The [charter](charter.md),
[safety model](safety.md), and [compatibility policy](compatibility.md) govern changes to
those boundaries.

## Published escape hatch

The safe `lean-rs` API hides raw runtime state. Applications that need an unwrapped capability may depend directly on
`lean-rs-sys` and accept its unsafe preconditions. Publishing the raw crate makes this possible without a workspace
fork; it does not add raw re-exports to `lean-rs` or promise safe behavior for FFI callers.

Declarations preserve C names so they can be compared directly with Lean's headers. Hand-written modules group the ABI
by function: initialization, memory, constructors, objects, strings, arrays, scalars, bignums, closures, IO, external
objects, and refcounts. The [source tree](https://github.com/jcreinhold/lean-rs/tree/main/crates/lean-rs-sys/src) is authoritative; file-count and symbol-count
estimates are not part of the design.

The curated surface stays hand-written. It has no bindgen/clang dependency and can be audited per symbol and supported
version. Adding an export requires updating its category module, the
[`REQUIRED_SYMBOLS`](https://github.com/jcreinhold/lean-rs/blob/main/crates/lean-rs-abi/src/symbols.rs) allowlist, and linkage coverage.

## Opaque public types

Public [`lean_object`](https://github.com/jcreinhold/lean-rs/blob/main/crates/lean-rs-sys/src/types.rs) is a zero-length opaque type with private
`PhantomData<(*mut u8, PhantomPinned)>`. It is `!Send + !Sync + !Unpin`; callers hold pointers rather than constructing
or copying objects. Ownership aliases retain Lean's ABI names.

The actual `#[repr(C)]` layouts, including `LeanObjectRepr` and subclass headers, live privately in
[`repr.rs`](https://github.com/jcreinhold/lean-rs/blob/main/crates/lean-rs-sys/src/repr.rs). Public unsafe helpers cast to those layouts internally. Downstream
code cannot write object fields through the published type, and private layout corrections do not expose new public
fields. Opacity does not discharge the raw caller's pointer, ownership, or thread preconditions.

`m_rc` is stored as `i32` for an exact C layout. Atomic access is introduced at the operation through
`AtomicI32::from_ptr`, rather than by changing the field type. Layout assertions for private structs live in that
module's unit tests; integration tests cover public ABI shapes without exposing private representations.

## Inline mirrors and refcounts

Lean's refcount helpers are `static inline`, so there is no runtime symbol to import. Rust mirrors inline directly into
`Obj` clone/drop paths and avoid a C shim or cross-language LTO requirement. The mirrors use relaxed atomic accesses;
`lean_dec_ref_cold` remains the external deallocation path. Single-threaded increments and decrements use strict
arithmetic to expose invariant violations in debug and release builds.

Each inline is a maintained ABI assumption, not generated evidence. A new header digest requires reviewing both layout
and inline semantics: unchanged struct offsets alone do not prove refcount equivalence. See the
[compatibility notes](compatibility.md) for version-specific changes, including the scalar-array
linearity marker.

## Discovery, headers, and linking

[`build.rs`](https://github.com/jcreinhold/lean-rs/blob/main/crates/lean-rs-sys/build.rs) discovers the Lean prefix, hashes `include/lean/lean.h`, and requires an
audited digest from `SUPPORTED_TOOLCHAINS`. It emits live version/header constants, exact-version and minor-lower-bound
`cfg` flags, link directives, and Cargo rerun inputs. `lean-rs-abi` is a real build dependency, so published packages do
not reach into sibling source directories for metadata. `sha2` performs the digest calculation.

Normal raw-FFI builds need Lean. `DOCS_RS=1` is the documentation-only exception: it emits synthetic metadata from the
supported table without probing or linking a runtime. This is not a runtime build mode. Metadata-only consumers use
`lean-rs-abi` or `lean-toolchain`; the legacy `metadata-only` feature on `lean-rs-sys` is a deprecated marker.

The default features are `dynamic` and `mimalloc`. Dynamic mode links `libleanshared`. Static mode is explicit and
mutually exclusive with dynamic mode; its archive set and order belong to the build script. `mimalloc` is currently a
compatibility marker because Lean bundles its allocator. No separate mimalloc library is added.

Runtime initialization symbols absent from `lean.h` still have explicit declarations and linkage tests. A header
checksum cannot validate those symbols or detect a different shared library substituted after compilation. The digest
check is an ABI compatibility guard, not a security boundary.

## Unsafe discipline and verification

`lean-rs-sys` is the workspace's sole crate-wide `unsafe_code` opt-out. Every unsafe block names its invariant in a
`// SAFETY:` comment; every public unsafe function has a `# Safety` section. Public FFI types never expose layout
fields. Pointer casts and allocation arithmetic remain local and justified; layout assumptions stay private.

The current workspace MSRV is declared in [`Cargo.toml`](https://github.com/jcreinhold/lean-rs/blob/main/Cargo.toml); it is 1.94. The inline mirrors use strict
arithmetic stabilized in 1.91, but that is no longer the workspace minimum.

Verification is split by what each check can prove:

- [`tests/linkage.rs`](https://github.com/jcreinhold/lean-rs/blob/main/crates/lean-rs-sys/tests/linkage.rs) takes addresses of required symbols and checks that
  its probe count matches the allowlist. It does not parse declarations to prove set equality.
- [`tests/layout.rs`](https://github.com/jcreinhold/lean-rs/blob/main/crates/lean-rs-sys/tests/layout.rs) and private repr tests pin layout assumptions.
- [`tests/safety_grep.rs`](https://github.com/jcreinhold/lean-rs/blob/main/crates/lean-rs-sys/tests/safety_grep.rs) checks the documented unsafe/public surface
  conventions lexically; it is not a soundness proof.
- Compile-fail fixtures pin opaque types and the unsafe call boundary.
- Miri exercises Rust-only pointer and atomic behavior; it cannot validate Lean's C runtime. See the
  [sanitizer workflow](https://github.com/jcreinhold/lean-rs/blob/main/.github/workflows/sanitizer.yml).
- The [toolchain bump procedure](../bump-toolchain.md) covers header/layout comparison and required-symbol probes.

Windows and toolchains outside the supported window have no compatibility commitment. Keep platform support and version
policy in [versioning and compatibility](compatibility.md).
