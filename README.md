# lean-rs

Rust bindings for hosting [Lean 4](https://lean-lang.org/) capabilities. Lean owns Lean semantics — elaboration, kernel
checking, proof objects, universes, `MetaM`, and dependent-type meaning. This project owns hosting: linking, runtime
initialization, ABI conversion, module loading, error and panic boundaries, scheduling, diagnostics, batching, and
packaging. Rust does not reconstruct Lean semantic facts; that responsibility stays in Lean.

The published surface centres on three crates. `lean-rs` is the single safe front door, with curated entry points at
`lean_rs::*` and specialized sub-modules for advanced consumers; `lean-toolchain` and `lean-rs-sys` exist for
build-script and raw-FFI escape hatches respectively.

## Workspace layout

Three published crates. Raw Lean 4 C ABI bindings live in the in-tree `lean-rs-sys` crate; the rest of the workspace
builds on top of it. See `docs/architecture/00-charter.md` for the layering charter and
`docs/architecture/05-raw-sys-design.md` for the `lean-rs-sys` design rationale.

| Crate            | Published | Role                                                                                                                                                                                                                                                                                                                                                   |
| ---------------- | --------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `lean-rs-sys`    | yes       | Raw Lean 4 C ABI bindings: curated `extern "C"` declarations split by semantic category, pure-Rust mirrors of `lean.h`'s `static inline` refcount helpers, `REQUIRED_SYMBOLS` allowlist, header digest. Public types (`lean_object`) are opaque; layout is `pub(crate)`. Opt-in unsafe raw FFI; the safe layers in `lean-rs` are the recommended path. |
| `lean-toolchain` | yes       | Lean toolchain discovery, typed fingerprint, fixture digest, layered link diagnostics, and build-script helpers that downstream embedders can call from their own `build.rs`.                                                                                                                                                                          |
| `lean-rs`        | yes       | The single safe front door: runtime initialization (token-bound `'lean` lifetime), owned/borrowed object handles (internal), typed ABI conversions (internal), module loading, typed exported functions, semantic handles, bounded meta services, and `LeanSession` bulk/pool operations.                                                              |

The layering invariant is `lean-rs-sys` → `lean-toolchain` → `lean-rs`. Raw `lean_object *` and raw `lean_*` symbols
enter the workspace only via `lean-rs-sys` and are not re-exported by `lean-toolchain` or `lean-rs`. Internal
organization of `lean-rs` is three publicly visible modules (`lean_rs::module`, `lean_rs::host`, `lean_rs::error`) plus
two `pub(crate)` infrastructure modules (`runtime`, `abi`); batch and pool operations are methods on `LeanSession`
rather than a sibling module. Boundaries are policed by `pub(crate)` rather than crate splits, so they can be
reorganized without semver breakage. See `docs/architecture/03-host-api.md` for the curated public surface.

## Layering and common path

Most embedders should follow this order from outermost to innermost:

1. **Depend on `lean-rs`** (`lean-rs = "0.1"` in your `Cargo.toml`) and use items at `lean_rs::*` — the curated entry
    points (`LeanRuntime`, `LeanHost`, `LeanCapabilities`, `LeanSession`, `LeanError`, `LeanResult`, the four semantic
    handle types, `LeanDiagnosticCode`, `DiagnosticCapture`, `SessionPool`, etc.) cover the happy path end to end.
1. **Drop into `lean_rs::module::*`** if you need to load a Lake-built dynamic library or call a typed Lean export
    that `lean-rs`'s session API does not yet wrap. The `LeanLibrary`, `LeanModule`, and
    `LeanExported<'lean, 'lib, Args, R>` types are escape hatches still inside the safe surface.
1. **Drop into `lean_rs::host::meta::*`** for the bounded `MetaM` capability (`LeanMetaService`, `LeanMetaResponse`,
    `LeanMetaOptions`, the three pinned service constructors `infer_type` / `whnf` / `heartbeat_burn`). This surface is
    intentionally not at the crate root: it is an optional capability — only callers that opt in to `LeanSession::run_meta`
    pay the namespace cost.
1. **Depend on `lean-toolchain` directly** only if your own `build.rs` needs Lean discovery, fingerprint, or link
    diagnostics without pulling in the safe runtime. Most application code never touches this crate.
1. **Depend on `lean-rs-sys` directly** only as a last-resort raw-FFI escape hatch. It is published per
    `RD-2026-05-17-005` with opaque public types and full `unsafe` discipline (every `pub unsafe fn` carries a
    `# Safety` section naming the invariant). The safe layers in `lean-rs` are strongly preferred; if the safe
    surface is missing a capability you need, contributing it upstream is preferable to reaching for raw FFI.

## Architecture

Architecture and policy docs live under [`docs/architecture/`](docs/architecture/):

- [`00-charter.md`](docs/architecture/00-charter.md) — design boundary, hidden knowledge, smallest public interface, and
    the design-it-twice record of rejected alternatives.
- [`01-safety-model.md`](docs/architecture/01-safety-model.md) — unsafe boundary thesis, reference-counting ownership,
    proof-object opacity, concurrency stance, and the workspace `unsafe-code = "deny"` lint policy.
- [`02-versioning-and-compatibility.md`](docs/architecture/02-versioning-and-compatibility.md) — supported Lean
    toolchain range, in-tree raw-FFI policy, header digest policy, crate semver, and supported-platform list.
- [`03-host-api.md`](docs/architecture/03-host-api.md) — curated public surface of `lean-rs`, classification table for
    the semver boundary, and the design-it-twice record for the surface shape.
- [`04-concurrency.md`](docs/architecture/04-concurrency.md) — the `!Send + !Sync` contract and worker-thread attach
    discipline.
- [`05-raw-sys-design.md`](docs/architecture/05-raw-sys-design.md) — per-decision rationale behind `lean-rs-sys`
    (publication status, opaque types, refcount-mirror strategy, module layout, naming, minimum-unsafe discipline).
- [`06-codegen-rationale.md`](docs/architecture/06-codegen-rationale.md) — the no-codegen decision for typed exported
    function handles, and the triggers that would re-open it.

The frozen public surface for each crate is recorded under [`docs/api-review/`](docs/api-review/); later changes diff
against those baselines.

## Diagnostics

Every error-bearing public type projects to the stable
[`LeanDiagnosticCode`](crates/lean-rs/src/error/mod.rs) taxonomy via
`.code()`, and the crate emits structured `tracing` spans against the
`lean_rs` target. See [`docs/diagnostics.md`](docs/diagnostics.md) for
the code catalogue, the span catalogue, the recommended `RUST_LOG`
scopes, and recipes for the in-process `DiagnosticCapture` test
affordance.

## Build

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo doc --no-deps
```

## License

Dual-licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or <http://www.apache.org/licenses/LICENSE-2.0>)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or <http://opensource.org/licenses/MIT>)

at your option. See [CONTRIBUTING.md](CONTRIBUTING.md) for contribution rules, including unsafe-code and
Lean-version-compatibility expectations.
