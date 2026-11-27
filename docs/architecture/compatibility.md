# Versioning and Compatibility

[`SUPPORTED_TOOLCHAINS`](https://github.com/jcreinhold/lean-rs/blob/main/crates/lean-rs-abi/src/supported.rs) is the source of truth for accepted Lean releases
and audited `lean.h` digests. The current window starts at 4.30.0 and ends at 4.36.0, including the explicitly listed
release candidates. Do not infer support for an unlisted release from its version number. The
[version matrix](../version-matrix.md) summarizes support; the [bump procedure](../bump-toolchain.md) owns changes to
it.

## Toolchain identity and ABI checks

| Crate | Owns |
| --- | --- |
| `lean-rs-abi` | Static `SUPPORTED_TOOLCHAINS`, `REQUIRED_SYMBOLS`, and layout literals; no build script, probe, FFI, or linking |
| `lean-rs-sys` | Raw declarations, inline mirrors, opaque types, private layouts, runtime linking, and build-baked live identity |
| `lean-toolchain` | Discovery, typed fingerprints, fixture digests, build/link diagnostics and helpers; its own build-baked live identity |

`lean-rs-sys` and `lean-toolchain` hash the discovered header and require a matching supported entry. A discovered but
unsupported header fails the build with the digest and accepted window. Header-identical releases share one entry. Both
build scripts record discovered and resolved versions; resolution prefers the exact discovered alias when listed and
otherwise uses the entry's first alias.

`lean-toolchain` can emit latest-supported metadata when no toolchain is installed. That fallback supports link-free
consumers; it is not evidence of an installed runtime. Normal `lean-rs-sys` builds require Lean. Under `DOCS_RS=1`, both
crates emit synthetic documentation metadata without probing Lean; raw-FFI documentation mode emits no runtime links.

`LeanRuntime::init` checks the build-baked version against the supported table. It cannot query the loaded Lean C
runtime's version and does not detect a shared-library substitution after compilation. Header checks guard audited
layout and inline assumptions; they are not a security boundary or a proof that every required runtime symbol exists.
Linkage tests and toolchain probes cover symbols separately.

A supported-window change must review object layouts, inline semantics, ownership conventions, initialization, shim
builds, and actual loading. A digest change alone need not mean an ABI break; an unchanged struct layout does not prove
that inline behavior stayed compatible. See [raw FFI design](raw-ffi.md).

## Compatibility evidence that explains the window

The table's source comments retain full digest and per-release audit details. These findings explain the non-obvious
boundaries and checks:

- A 2026-05-18 sweep of 4.23.0–4.29.1 found service-layer crashes in `lean_dec_ref_cold` on releases through 4.25.x;
  4.26.0 and later passed that sweep. The floor subsequently moved to 4.27.0 on 2026-07-19 because 4.26.0 could not
  build the bundled shims (`ContextInfo.cmdEnv?`, `String.trimAscii`, and an environment add-declaration type mismatch).
- The floor moved to 4.30.0 on 2026-07-28 because 4.27.0–4.29.1 could not load the host shim that uses
  `Lean.collectAxioms`. `nm -gU` found no `collectAndGet` exports in 4.29.1 and four in 4.30.0 and later. The missing
  symbol was `_l___private_Lean_Util_CollectAxioms_0__Lean_CollectAxioms_collectAndGet___boxed`. A successful header
  comparison had therefore been insufficient evidence of usable host services.
- 4.31.0 final has a different header from its release candidates and needs its own digest row. Header-identical
  releases, such as 4.32.0-rc1 / 4.32.0 / 4.32.2, share a row.
- 4.33.0 adds C11 `_Atomic` qualifiers to `lean_task_imp.m_canceled` and `lean_task_object.m_imp`. The recorded probe
  found identical sizes, alignments, and field offsets for the ten mirrored structs. A textual header diff remains
  nonempty even when the measured layout is unchanged.
- 4.34.0-rc1 adds TSan refcount instrumentation; 4.33.1 backports it. 4.34.0-rc2 adds sticky-refcount bands, wraparound
  increment logic, `lean_inc_ref_huge_n`, and `lean_nat_size_in_bytes`. Later 4.34.1 / 4.35.0-rc3 headers factor that
  logic through `lean_is_unstuck_mt` and add `lean_is_never_freed` / `LEAN_RC_STUCK_ST`. These are reasons to review
  inline behavior separately from layout and symbol availability.
- 4.35.0-rc1 assigns `0x80` of `m_other` to `LEAN_LINEAR_MARK_MASK`. The scalar-array element-size mirror masks that
  bit; older releases leave it unset. The release also renames/copies helpers outside the bound surface. 4.35.0-rc2's
  `lean_nat_powmod` addition is outside `REQUIRED_SYMBOLS`.

Routine new aliases and qualification dates belong in the supported table and git history rather than a second
release-by-release chronology here.

## CI coverage and platforms

The [CI workflow](https://github.com/jcreinhold/lean-rs/blob/main/.github/workflows/ci.yml) runs the head Lean version on `ubuntu-latest` and `macos-latest` by
default. Its manual `full_matrix` option runs every listed release on both platforms. A support claim must distinguish
head validation from full-window validation; the default PR run is not a full-window sweep.

Supported platforms are x86_64 GNU/Linux and Apple Silicon macOS as exercised by that matrix. Windows, BSD, embedded,
and WASM have no support commitment. Adding a platform requires its build/link policy and CI coverage, not just a
conditional compilation fix.

The Rust channel is `stable` in [`rust-toolchain.toml`](https://github.com/jcreinhold/lean-rs/blob/main/rust-toolchain.toml); this follows stable rather than
pinning an exact compiler release. The MSRV is 1.94 in [`Cargo.toml`](https://github.com/jcreinhold/lean-rs/blob/main/Cargo.toml). Compile-fail diagnostics are
validated in their dedicated [workflow](https://github.com/jcreinhold/lean-rs/blob/main/.github/workflows/compile-fail.yml) because wording varies with compiler
and platform.

## Lake naming

All supported Lean releases use the newer Lake convention. The loader and project discovery still probe legacy filenames
so callers need not encode the naming policy themselves:

| Lean range | Shared library | Initializer |
| --- | --- | --- |
| Through 4.26 | `lib{LibName}.{dylib,so}` | `initialize_{MangledModule}` |
| From 4.27 | `lib{escaped_package}_{LibName}.{dylib,so}` | `initialize_{MangledPackage}_{MangledModule}` |

Probing the old shape does not extend the supported toolchain window. Use the
[build/link helpers](build-and-link.md), not hand-built paths.

## Crate and protocol compatibility

Cargo `0.x` semver governs public Rust items at crate-root and public module paths; a minor version may break APIs.
Private modules and `pub(crate)` items may be reorganized without creating a public path commitment. The
[API review baselines](../api-review.md) record the reviewed surface.

`lean-rs-sys` commits to public symbol names/signatures and its toolchain window, with unsafe caller obligations.
Private header layouts are not public fields. Bundled generic and host Lean packages must work within the same window.
The worker protocol also checks its own version at the process boundary; use matched parent/child releases rather than
assuming arbitrary wire compatibility across protocol changes.

When extending the Lean window, follow [the bump procedure](../bump-toolchain.md), update the table and CI matrix, and
run the full-window checks. If a layout, ownership, initializer, or shim assumption breaks, resolve that contract change
before claiming support.
