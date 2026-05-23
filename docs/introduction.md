# lean-rs

Rust bindings for hosting [Lean 4](https://lean-lang.org/) capabilities. Lean owns Lean semantics — elaboration, kernel
checking, proof objects, universes, `MetaM`, dependent-type meaning. `lean-rs` owns hosting: linking, runtime
initialization, ABI conversion, module loading, error and panic boundaries, scheduling, diagnostics, batching, and
packaging. Rust does not reconstruct Lean semantic facts.

This site collects the architecture charter, design notes, recipes, safety audits, and operational runbooks that ship in
the [`lean-rs` repository](https://github.com/jcreinhold/lean-rs). The
[repository README](https://github.com/jcreinhold/lean-rs#readme) is the entry point for new users — it walks through
the crate layout, the minimal same-process example, and the worked examples. Read it first if you have not used
`lean-rs` before.

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
- **Host stack (`lean-rs-host`)** — the L2 theorem-prover-host surface and its capability contract.
- **Worker (`lean-rs-worker`)** — the process-boundary supervisor and its scale and observability story.
- **Recipes** — task-oriented walkthroughs for shipping a Lean-backed crate or wiring a worker capability.
- **Safety audits** — long-session memory bounds and the workspace `unsafe` inventory.
- **Operations** — testing strategy, performance baselines, diagnostics, release process, toolchain bumps.

## Versions and platforms

`lean-rs` targets stable Rust (MSRV 1.91) on macOS and Linux. The supported Lean toolchain window is enumerated in
[Version matrix](version-matrix.md); the procedure for extending it is [Bump toolchain](bump-toolchain.md).
