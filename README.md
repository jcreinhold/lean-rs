# lean-rs

[![CI](https://github.com/jcreinhold/lean-rs/actions/workflows/ci.yml/badge.svg)](https://github.com/jcreinhold/lean-rs/actions/workflows/ci.yml)
[![Sanitizer](https://github.com/jcreinhold/lean-rs/actions/workflows/sanitizer.yml/badge.svg)](https://github.com/jcreinhold/lean-rs/actions/workflows/sanitizer.yml)
[![Release](https://github.com/jcreinhold/lean-rs/actions/workflows/release.yml/badge.svg)](https://github.com/jcreinhold/lean-rs/actions/workflows/release.yml)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT_OR_Apache--2.0-blue.svg)](#license)

Rust bindings for hosting [Lean 4](https://lean-lang.org/) capabilities. Lean owns Lean semantics — elaboration, kernel
checking, proof objects, universes, `MetaM`, and dependent-type meaning. This project owns hosting: linking, runtime
initialization, ABI conversion, module loading, error and panic boundaries, scheduling, diagnostics, batching, and
packaging. Rust does not reconstruct Lean semantic facts; that responsibility stays in Lean.

The published surface is **four crates**. `lean-rs` is the typed-FFI primitive — the (β)-binding minimum every embedder
needs to call any `@[export]` Lean function from Rust. `lean-rs-host` is an opinionated theorem-prover-host application
framework built on top of `lean-rs`; it ships its own 13+3 Lean shim contract. `lean-toolchain` and `lean-rs-sys` exist
for build-script and raw-FFI escape hatches respectively. Per `RD-2026-05-18-001`, the L1 (`lean-rs`) / L2 (`lean-rs-host`)
split is the project's structural recognition that Lean is an OCaml-shaped (β) FFI target — no mainstream Rust-binding
crate to such a language ships pre-compiled target-language helper code, and the opinionated host stack is one example
of how to use the L1 primitive, not the only way.

## Workspace layout

| Crate            | Published | Role                                                                                                                                                                                                                                                                                                                                                                                   |
| ---------------- | --------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `lean-rs-sys`    | yes       | Raw Lean 4 C ABI bindings: curated `extern "C"` declarations split by semantic category, pure-Rust mirrors of `lean.h`'s `static inline` refcount helpers, `REQUIRED_SYMBOLS` allowlist, header digest. Public types (`lean_object`) are opaque; layout is `pub(crate)`. Opt-in unsafe raw FFI; the safe layers in `lean-rs` are the recommended path.                                  |
| `lean-toolchain` | yes       | Lean toolchain discovery, typed fingerprint, fixture digest, layered link diagnostics, and build-script helpers that downstream embedders can call from their own `build.rs`.                                                                                                                                                                                                          |
| `lean-rs`        | yes       | **L1 FFI primitive.** Runtime initialization (token-bound `'lean` lifetime), owned/borrowed object handles, typed ABI conversions, module loading, typed exported functions, semantic handles (`LeanName`/`LeanLevel`/`LeanExpr`/`LeanDeclaration`), structured error/diagnostic boundary. Ships zero Lean-side code; the (β)-binding analog of `ocaml-rs`.                              |
| `lean-rs-host`   | yes       | **L2 opinionated host stack.** `LeanHost` / `LeanCapabilities` / `LeanSession` trio, kernel-checked `LeanEvidence` and `ProofSummary`, bounded `MetaM` service registry, `SessionPool` / `PooledSession`. Requires a 13+3 `lean_rs_host_*` Lean shim contract in the capability dylib it loads.                                                                                          |

The layering invariant is `lean-rs-sys` → `lean-toolchain` → `lean-rs` → `lean-rs-host`. Raw `lean_object *` and raw
`lean_*` symbols enter the workspace only via `lean-rs-sys` and are not re-exported by `lean-toolchain` or `lean-rs`.
The L1 (`lean-rs`) curated surface is the typed FFI primitive plus the four core semantic handle types and the error
boundary; the L2 (`lean-rs-host`) curated surface is the opinionated theorem-prover-host capability stack. See
`docs/architecture/04-host-stack.md` for the L2 classification table.

## Layering and common path

Most embedders should follow this order from outermost to innermost:

1. **Depend on `lean-rs`** (`lean-rs = "0.1"` in your `Cargo.toml`) for the typed-FFI primitive. Items at `lean_rs::*` —
    `LeanRuntime`, `LeanLibrary`, `LeanModule`, `LeanExported`, the four handle types, `LeanError`, `LeanResult`,
    `LeanDiagnosticCode`, `DiagnosticCapture` — cover any downstream that wants to call `@[export]` Lean functions
    from Rust. No Lean-side shim contract; you write your own `@[export]` declarations for the operations you need.
1. **Drop into `lean_rs::module::*`** or `lean_rs::abi::*` for the typed exported-function dispatch primitives
    (`LeanExported`, `LeanArgs`, `DecodeCallResult`, `LeanIo`) and the sealed `LeanAbi` trait that drives them.
1. **Add `lean-rs-host`** (`lean-rs-host = "0.1"`) if you want the curated theorem-prover-host capability stack
    (`LeanHost`, `LeanCapabilities`, `LeanSession`, `SessionPool`, `LeanEvidence`, `ProofSummary`, the elaboration
    /`MetaM` surfaces). This crate requires a 13+3 `lean_rs_host_*` `@[export]` Lean shim contract in your capability
    dylib; the shim sources live in `fixtures/lean/LeanRsFixture/{Environment,Elaboration,Meta}.lean` today
    (their packaging as a shipping artifact for external consumers is prompt-30 hardening work).
1. **Drop into `lean_rs_host::meta::*`** for the bounded `MetaM` capability (`LeanMetaService`, `LeanMetaResponse`,
    `LeanMetaOptions`, the three pinned service constructors `infer_type` / `whnf` / `heartbeat_burn`). Sub-module-only
    because the optional `MetaM` capability would pollute the crate root for callers who never use `LeanSession::run_meta`.
1. **Depend on `lean-toolchain` directly** only if your own `build.rs` needs Lean discovery, fingerprint, or link
    diagnostics without pulling in the safe runtime. Most application code never touches this crate.
1. **Depend on `lean-rs-sys` directly** only as a last-resort raw-FFI escape hatch. It is published per
    `RD-2026-05-17-005` with opaque public types and full `unsafe` discipline (every `pub unsafe fn` carries a
    `# Safety` section naming the invariant). The safe layers in `lean-rs` and `lean-rs-host` are strongly preferred.

## Architecture

Architecture and policy docs live under [`docs/architecture/`](docs/architecture/):

- [`00-charter.md`](docs/architecture/00-charter.md) — design boundary, hidden knowledge, smallest public interface, and
    the design-it-twice record of rejected alternatives.
- [`01-safety-model.md`](docs/architecture/01-safety-model.md) — unsafe boundary thesis, reference-counting ownership,
    proof-object opacity, concurrency stance, and the workspace `unsafe-code = "deny"` lint policy.
- [`02-versioning-and-compatibility.md`](docs/architecture/02-versioning-and-compatibility.md) — supported Lean
    toolchain range, in-tree raw-FFI policy, header digest policy, crate semver, and supported-platform list.
- [`04-host-stack.md`](docs/architecture/04-host-stack.md) — curated public surface of `lean-rs-host` (the L2
    opinionated theorem-prover-host stack), classification table for its semver boundary, and the design-it-twice
    record for the surface shape. The L1 `lean-rs` curated surface is recorded in
    `docs/api-review/lean-rs-public.txt`.
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
