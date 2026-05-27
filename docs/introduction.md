# lean-rs

A Lean runtime bridge for Rust. The workspace has three layers: a typed FFI layer (`lean-rs`), a standard Lean service
layer (`lean-rs-host`), and an optional process-isolation layer (`lean-rs-worker-*`). Lean owns Lean semantics:
elaboration, kernel checking, proof objects, universes, `MetaM`, and dependent-type meaning. The Rust bridge owns the
hosting work around those semantics: linking, runtime initialization, ABI conversion, module loading, error and panic
boundaries, scheduling, diagnostics, batching, process isolation, and packaging. Rust does not reconstruct Lean semantic
facts.

This site collects the architecture charter, design notes, recipes, safety audits, and operational runbooks that ship in
the [`lean-rs` repository](https://github.com/jcreinhold/lean-rs). New users should start with the
[repository README](https://github.com/jcreinhold/lean-rs#readme), which walks through the crate layout, the minimal
same-process example, and the worked examples.

## Where to start

If you read only two pages on this site, read them in this order:

1. [Charter](architecture/00-charter.md) — the design boundary between Lean and `lean-rs`, what is hidden, what is
   preserved, and which alternatives were rejected.
1. [Safety model](architecture/01-safety-model.md) — the unsafe boundary, refcount ownership, and the workspace
   concurrency stance.

Everything else is reference for a specific subsystem. The numeric prefix on each architecture document reflects the
order it was written, not the order it should be read; use the sidebar groupings instead.

## How the site is organized

- **Foundations** — charter, safety model, versioning, raw FFI rationale.
- **Same-process FFI (`lean-rs`)** — the L1 safe front door: concurrency, panic containment, callbacks, loader.
- **Standard Lean services (`lean-rs-host`)** — the L2 service surface and its capability contract.
- **Worker (`lean-rs-worker-protocol` / `-parent` / `-child`)** — the process-boundary supervisor and its scale and
  observability story.
- **Recipes** — task-oriented walkthroughs for shipping a Lean-backed crate or wiring a worker capability.
- **Safety audits** — long-session memory bounds and the workspace `unsafe` inventory.
- **Operations** — testing strategy, performance baselines, diagnostics, release process, toolchain bumps.

## Versions and platforms

`lean-rs` targets stable Rust (MSRV 1.91) on macOS and Linux. The supported Lean toolchain window is enumerated in
[Version matrix](version-matrix.md); the procedure for extending it is [Bump toolchain](bump-toolchain.md).
