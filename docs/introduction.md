# lean-rs

A Lean runtime bridge for Rust. Use it to load Lean-built shared libraries, call typed `@[export]` functions, run
standard Lean services such as elaboration and kernel checking, or isolate Lean work in a child process.

Lean remains the authority for elaboration, kernel checking, proof objects, universes, `MetaM`, and dependent-type
meaning. This project handles the Rust hosting work around Lean: linking, runtime initialization, ABI conversion, module
loading, error and panic boundaries, scheduling, diagnostics, batching, process isolation, and packaging.

This site collects the architecture charter, design notes, recipes, safety audits, and operational runbooks that ship in
the [`lean-rs` repository](https://github.com/jcreinhold/lean-rs). New users should start with the
[repository README](https://github.com/jcreinhold/lean-rs#readme), which walks through the crate layout, the minimal
same-process example, and the worked examples.

## Start Here

For a runnable project path, start with the [repository README](https://github.com/jcreinhold/lean-rs#readme). It shows
the crate layout, a minimal same-process example, and the worked examples.

If you are reviewing the design, read these next:

1. [Charter](architecture/00-charter.md)—the design boundary between Lean and `lean-rs`, what is hidden, what is
   preserved, and which alternatives were rejected.
1. [Safety model](architecture/01-safety-model.md)—the unsafe boundary, refcount ownership, and the workspace
   concurrency stance.

Everything else is reference for a subsystem. The numeric prefix on each architecture document is an identifier, not a
reading order; use the sidebar groupings instead.

## How the site is organized

- **Foundations**—charter, safety model, versioning, raw FFI rationale.
- **Same-process FFI (`lean-rs`)**—runtime, typed exports, concurrency, panic containment, callbacks, loader.
- **Standard Lean services (`lean-rs-host`)**—sessions, elaboration, kernel checking, `MetaM`, capability contract.
- **Worker (`lean-rs-worker-protocol` / `-parent` / `-child`)**—the process-boundary supervisor and its scale and
  observability story.
- **Recipes**—task-oriented walkthroughs for shipping a Lean-backed crate or wiring a worker capability.
- **Safety audits**—long-session memory bounds and the workspace `unsafe` inventory.
- **Operations**—testing strategy, performance baselines, diagnostics, release process, toolchain bumps.

## Versions and platforms

`lean-rs` targets stable Rust (MSRV 1.91) on macOS and Linux. The supported Lean toolchain window is enumerated in
[Version matrix](version-matrix.md); the procedure for extending it is [Bump toolchain](bump-toolchain.md).
