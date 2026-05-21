# Worker Pool

Prompt 76 hardened the single-worker production boundary. A single worker is
the right unit for panic containment and process-global memory reset, but it is
not enough for mathlib-scale or multi-package workloads. Those workloads need
parallelism, warm session reuse, memory admission, and failure isolation across
more than one child process.

The pool boundary keeps that operational machinery inside `lean-rs-worker`.
Downstream callers submit capability work keyed by session requirements. The
pool decides whether to reuse a warm worker, start a new child, cycle a stale
child, or delay work until policy permits it.

## Chosen Boundary

`LeanWorkerPool` is the local multi-worker orchestration boundary. It owns:

- worker child lifecycle;
- work queueing and admission;
- session leases and session-key matching;
- worker restart and session invalidation sequencing;
- memory-aware scheduling and RSS sampling policy;
- failure isolation between child processes;
- cancellation and timeout propagation into leased work;
- pool-level observability.

The pool does not replace `LeanWorkerCapabilityBuilder` or typed commands. It
sits above them: the builder describes how to open one capability-backed worker
session, while the pool decides which local child should host that session for
one piece of work.

Prompt 78 implements the first public surface as a lease-first API:

- `LeanWorkerPool` owns a bounded set of local capability workers;
- `LeanWorkerPoolConfig` currently exposes a fixed `max_workers` limit;
- `LeanWorkerSessionKey` records the worker reuse facts;
- `LeanWorkerSessionLease` runs typed JSON and streaming commands without
  exposing `LeanWorkerSession` as the primary pool API.

A lease becomes invalid after timeout, cancellation, child fatal exit,
explicit cycle, or capability metadata mismatch. The caller acquires a fresh
lease for follow-up work. That rule keeps session invalidation explicit without
making callers learn which child process or warm worker was selected.

## Designs Considered

**Single worker only.** Rejected as the scale foundation. It preserves a clean
process boundary, but it serializes independent module groups. A downstream
tool that wants to use available cores would have to build its own worker
fanout, queueing, and restart policy.

**Caller-managed worker fanout.** Rejected. It would push child counts, restart
timing, session reuse, memory ceilings, lease invalidation, and failure
classification into every downstream tool. That duplicates the same production
rules that `lean-rs-worker` already owns for one worker.

**`LeanWorkerPool`.** Chosen. The pool is a deeper module because it hides
orchestration decisions that every local production consumer would otherwise
reimplement. The public surface should describe capability work and policy
intent, not child process mechanics.

Remote workers are out of the current arc. The local pool should not expose
APIs that assume a worker is identified by a child pid or a local pipe, because
that would make a future remote backend harder. The first supported backend is
local child processes.

## Session Keys

Pool reuse is keyed by the facts that make a worker session safe and useful to
reuse:

- Lake project root;
- capability package and target;
- import set;
- capability metadata expectations;
- Lean toolchain and `lean-rs` protocol fingerprint;
- restart-policy class.

The key is not a downstream cache key. Downstream crates still decide whether a
row, index, probe, or report is semantically valid. The pool key only answers a
worker question: can this already-open child session run the next compatible
request without repeating setup or violating policy?

The prompt-78 key records capability metadata expectations as opaque generic
metadata facts. `lean-rs-worker` can compare the expected and actual metadata
envelopes, but it does not interpret downstream command versions or decide
cache invalidation.

## What The Pool Hides

Callers should not learn:

- worker ids or child pids;
- stdin/stdout pipes or frame order;
- child spawn and handshake sequencing;
- queue internals;
- stderr parsing or fatal-exit classification;
- restart timing;
- RSS sampling details;
- which session values are invalidated by a cycle;
- which warm worker is selected for a request.

Those details are volatile and operational. Keeping them private lets
`lean-rs-worker` change scheduling, memory policy, batching, and future backend
internals without rewriting downstream tools.

## What Callers Still Know

Callers still own the facts that are part of their domain:

- capability identity;
- typed command request, row, and summary schemas;
- request values;
- timeout and cancellation intent;
- row commit policy after terminal success;
- row semantics, cache validity, ranking, reporting, and source provenance.

This is the same boundary as the worker capability layer. `lean-rs-worker`
owns process and transport behavior; downstream crates own semantic commands
and schemas. A `lean-dup` integration would map its own commands onto the pool,
but `lean-rs-worker` should not grow first-party `extract`, `features`,
`index`, or `probe` methods.

## Failure And Memory Model

A child crash, request timeout, cancellation-triggered cycle, explicit cycle,
or policy restart invalidates the sessions hosted by that child. The pool must
report those outcomes distinctly so callers can distinguish downstream
command failure from infrastructure failure.

Only process exit resets Lean process-global runtime and import state.
`SessionPool::drain()` can release Rust-owned cached environments inside a
child, but it is not an RSS reset. Pool memory policy therefore operates by
admitting work, selecting warm sessions carefully, and cycling child processes
when measured policy says a reset is needed.

## Next Prompts

Prompt 79 adds memory-aware scheduling on top of the lease-first API. Prompt 80
adds import-set planning so callers can produce stable work batches that make
pool reuse effective. Prompts 81-87 then harden batching, data-plane choices,
Lean-side streaming helpers, mathlib-scale fixtures, observability, downstream
readiness, and the final scale contract.
