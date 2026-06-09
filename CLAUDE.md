# lean-rs

A Rust binding stack for hosting Lean 4 capabilities. Lean owns elaboration, kernel checking, proof objects, universes,
`MetaM`, and dependent-type meaning; this project owns linking, runtime initialization, ABI conversion, module loading,
error and panic boundaries, scheduling, diagnostics, batching, and packaging.

## Read first

Before writing code, read the architecture charter and any topic-specific docs that govern the area you are touching:

1. [`docs/architecture/00-charter.md`](docs/architecture/00-charter.md)—the design boundary between Lean and `lean-rs`,
   hidden knowledge, preserved capability, and rejected alternatives.
1. [`docs/architecture/01-safety-model.md`](docs/architecture/01-safety-model.md)—the workspace unsafe policy.
1. The topic doc that owns the area you are changing (host stack, callbacks, worker, etc.). See the index in
   [`README.md`](README.md).

## Workspace shape

Nine published crates plus two workspace-internal helpers:

| Crate | Role |
| --- | --- |
| `lean-rs-sys` | Raw Lean 4 C ABI bindings: extern declarations, opaque public types, `#[repr(C)]` layout mirrors (`pub(crate) LeanObjectRepr`), pure-Rust refcount mirrors. Opt-in unsafe raw FFI. |
| `lean-rs-abi` | Link-free Lean ABI and toolchain metadata: `REQUIRED_SYMBOLS` allowlist, `LEAN_VERSION`, `LEAN_HEADER_DIGEST`, and the `SUPPORTED_TOOLCHAINS` window. No `extern "C"`, no linker directives. |
| `lean-toolchain` | Toolchain discovery, typed fingerprint, fixture digest, link diagnostics, Lake module discovery, build helpers. Re-exports `lean-rs-abi`'s metadata. |
| `lean-rs` | L1 safe front door. Runtime, object handles, ABI conversions, module loading, exported functions, semantic handles, callbacks, error boundary. |
| `lean-rs-interop-shims` | Package-owned Lean source for the generic interop shims; `materialize_source_package` copies the `LeanRsInterop` Lake package into a caller-owned build root. |
| `lean-rs-host` | L2 theorem-prover-host stack: `LeanHost` / `LeanCapabilities` / `LeanSession`, kernel-checked evidence, bounded `MetaM`, session pool. |
| `lean-rs-worker-protocol` | Wire-stable parent/child IPC types and frame codec. Does not link `libleanshared`. |
| `lean-rs-worker-parent` | Parent-side supervisor and pool: child lifecycle, request timeouts, memory cycling, typed commands, row streaming. Does not link `libleanshared`. |
| `lean-rs-worker-child` | Child runtime and the `lean-rs-worker-child` binary; the only worker crate that links `libleanshared`. |

Workspace-internal (`publish = false`): the `lean-rs-profiling` harness and the `lean-rs-fuzz` target.

Layering: `lean-rs-sys` → `lean-toolchain` → `lean-rs` → `lean-rs-host`, with `lean-rs-abi` underneath as the link-free
metadata crate. The worker boundary is three sibling crates: `-protocol` (wire types), `-parent` (supervisor and pool),
`-child` (Lean-linked runtime). Raw `lean_*` symbols enter only through `lean-rs-sys`; the safe layers in `lean-rs` and
above never re-export them. Advanced users who need raw FFI can depend on `lean-rs-sys` directly (opt-in unsafe).

## Build and verify

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo nextest run --workspace
cargo test --doc --workspace
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace
```

CI runs the same commands on `ubuntu-latest` and `macos-latest`, stable Rust only. `cargo test` (single-process) is
**not** the gate: cumulative Lean state OOMs the binary after ~150 tests. See [`docs/testing.md`](docs/testing.md) for
the rationale and the per-test debugging escape hatch.

## Discipline

- **Raw `lean_*` symbols enter the workspace only via `lean-rs-sys`.** If a symbol is missing or has a different
  signature in the active Lean header, extend the extern declarations in the appropriate
  `crates/lean-rs-sys/src/<category>.rs` file and the `REQUIRED_SYMBOLS` allowlist in `crates/lean-rs-abi/src/symbols.rs`.
  To extend the supported Lean toolchain window, follow `docs/bump-toolchain.md`: add a row to
  `crates/lean-rs-abi/src/supported.rs` (`SUPPORTED_TOOLCHAINS`), a CI matrix cell, and (if layout shifted) the
  `pub(crate) LeanObjectRepr` in `crates/lean-rs-sys/src/repr.rs`.
- **`lean-rs-sys` is published with opaque public types.** Downstream users see `lean_object` as `[u8; 0] + PhantomData`
  and reach state only through `pub unsafe fn` helpers. Never expose `LeanObjectRepr` outside the crate; never add
  public `pub` fields to FFI types. Every `unsafe { ... }` block carries a `// SAFETY:` comment; every `pub unsafe fn`
  carries a `# Safety` section.
- **No broad `pub use` facade.** Re-exports at `lean_rs::*` are curated public API, not path-shortening.
- **No speculative traits with one implementor.** Add a trait when a second concrete type needs it.
- **No `unwrap()`, `expect()`, or `panic!`** in non-test code unless a comment names a proof obligation.
- **`unsafe-code = "deny"`** at workspace level. `lean-rs-sys` is the one crate-wide opt-out; new `unsafe` elsewhere
  needs justification, a `// SAFETY:` comment naming the invariant, and reviewer sign-off.
- **No `TODO`, `unimplemented!()`, `todo!()`.** Build the intended functionality or stop.
- **Fix bugs at their root.** If the cause lives in a different module, fix it there.
- **Update the relevant architecture doc** in the same PR when its design changes.

## When this file is wrong

This file should drift slowly. If a session reveals something here is stale, fix it in the same PR. The same rule
applies to architecture docs and contract claims.
