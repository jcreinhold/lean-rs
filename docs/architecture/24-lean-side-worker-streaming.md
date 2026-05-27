# Lean-Side Worker Streaming Helpers

Worker capability authors should not repeat the JSON envelope details needed by the worker crates, but the helper
package must not become a downstream command framework. The stable boundary is small Lean-side primitives in
`LeanRsInterop.Worker.Stream`:

- `jsonString` escapes a Lean `String` as JSON text.
- `row` builds a worker row envelope with caller-owned stream and payload JSON.
- `diagnostic` builds a diagnostic envelope.
- `progress` and `chunkProgress` build progress envelopes.
- `metadata` builds the terminal metadata envelope.
- `emitAll` calls the in-process string callback trampoline inside the child process. Capability exports should build an
  array of envelopes locally and call `emitAll` once.
- `countChunks` computes chunk counts for downstream-owned chunk plans.

These helpers own worker callback-envelope mechanics. Downstream Lean code still owns request parsing, row schemas,
semantic algorithms, command names, chunk contents, and terminal metadata shape. The Rust worker and pool still own
worker selection, lease invalidation, memory policy, backpressure, cancellation, and timeout behavior.

## Packaging Boundary

The helpers live in `lean-rs-interop-shims`, not in `lean-rs-host-shims`. That package is the reusable ABI layer for
downstream Lean capabilities. It has no theorem-prover host policy, no declaration-query API, no session key logic, and
no worker scheduling policy.

Worker parent-facing APIs still expose typed commands, rows, diagnostics, progress, summaries, and timeouts. They do not
expose callback handles. `LeanCallbackHandle<LeanStringEvent>` stays inside the child as the bridge from Lean exports to
private worker frames.

## Chunking Decision

The verified helper path is envelope construction plus one callback loop per export. The fixture emits a chunked stream
by building an array of envelopes with `row`, `chunkProgress`, `diagnostic`, and `metadata`, then calling `emitAll`
once. Single-envelope convenience functions are intentionally absent: the active worker shared-library path is stable
when the downstream capability constructs the envelope array and hands it to `emitAll`.

A generic Lean-side chunk runner was tested and rejected. Runtime helpers that looped over `Array String` in
`lean-rs-interop-shims`, source macros that expanded the same loop, and single-envelope helpers that allocated a
one-element array in the shared shim package caused the worker child to terminate with SIGSEGV under Lean 4.29.1 in the
shared library worker path. The safe release surface therefore keeps chunk scheduling in downstream Lean code and
factors only worker envelope construction plus `emitAll` into the helper package.

This is not a pool scheduling feature. Real parallelism for large workloads belongs in `LeanWorkerPool`, session
leasing, import planning, and memory-aware admission. Lean-side helpers can emit progress at chunk boundaries, but they
do not choose workers, spawn tasks, or define session keys.

## Consumer Rule

Use these helpers when authoring a Lean capability that will be run through the worker crates. They remove repeated
worker-envelope boilerplate while keeping downstream schemas and algorithms downstream-owned.

Use direct `lean-rs` callbacks only for same-process interop that accepts the in-process Lean failure model. Use
`lean-rs-host` for checked in-process theorem-prover sessions. Use the worker crates or `LeanWorkerPool` when the caller
needs process isolation, timeouts, memory cycling, row streaming, diagnostics, or pool orchestration.
