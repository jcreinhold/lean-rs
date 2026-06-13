# Worker Runtime Semantics

The worker crates use actor-like boundaries, but they are not a formal actor runtime. The public contract is a
synchronous process supervisor and lease pool around local child processes. This document names the semantics callers
can rely on.

## Runtime Shape

A worker child is the process-isolation unit. `LeanWorker` owns the child process, stdio pipes, request timeout, restart
policy, lifecycle counters, and fatal-exit reporting. The child owns one `lean-rs-host` session at a time and processes
framed requests serially from stdin.

`LeanWorkerPool` is a local lease manager above those workers. It owns a bounded set of child processes, reuses warm
sessions by `LeanWorkerSessionKey`, applies RSS and idle policy before assignment, and returns a borrowed
`LeanWorkerSessionLease` for one compatible worker entry. The pool does not expose child ids, pids, pipes, frame order,
or which warm child was selected.

This is deliberately sync-first. The API uses `&mut LeanWorker`, `&mut LeanWorkerPool`, and borrowed leases instead of a
cloneable actor handle with a public `send` or `call` mailbox.

## Requests

Each `LeanWorker` has at most one parent request in flight. A request follows this path:

1. The parent checks cancellation and restart/RSS policy.
2. The parent writes one framed `Request` to the child.
3. The child handles that request in its serial stdio loop.
4. The child may emit diagnostics, progress events, and data rows.
5. The child emits one terminal `Response`, exits, or stops producing frames.

Delivery is at-most-once at the worker boundary. A successful write means the request entered the child IPC stream; it
does not mean the request was processed. A terminal success response is the success acknowledgement. Timeout,
parent-side cancellation after dispatch, RSS hard-limit kill, EOF, or fatal child exit can leave the caller unable to
know which child-side side effects happened before replacement. Automatic retry is therefore safe only for operations
whose downstream semantics are idempotent or otherwise deduplicated.

## Ordering

Within one worker child, requests are serial. Intermediate frames for one request are observed in pipe order until the
terminal response or failure. Streaming rows carry a per-stream sequence number assigned inside the child for that
request.

There is no global FIFO order across workers. The pool does not publish a scheduler, queue position, worker id, or
fairness guarantee. Callers that need semantic ordering across independent leases must impose it in their own domain
logic.

## Admission And Capacity

The pool capacity is the configured `max_workers`, interpreted as a maximum number of distinct local child workers. A
cold session key is admitted only when that limit and the configured RSS budget permit a new worker. A full pool returns
`WorkerPoolExhausted` immediately by default.

`queue_wait_timeout` is a bounded synchronous wait at the admission point. It is not a mailbox. While waiting, the pool
does not enqueue the request or reserve a queue slot; if the timeout expires, the caller receives
`WorkerPoolQueueTimeout`.

`LeanWorkerPoolSnapshot::queue_depth` is currently `0`. It is present as a stable snapshot field, not as evidence of an
implemented pool request queue.

Worker protocol frames are bounded by the negotiated per-frame cap. Streaming requests also use a bounded internal event
buffer between the supervisor reader thread and the request owner. When that buffer fills, the reader blocks, which
pushes back through the pipe instead of allowing unbounded parent memory growth. Rows are not dropped by this buffer,
but rows delivered before terminal success remain tentative.

## Failure And Restart

Only child process exit resets Lean process-global runtime and import state. Policy cycling, request timeout,
parent-side cancellation observed after dispatch, in-flight RSS hard limits, fatal child exit, and explicit cycle all
replace the child process and invalidate affected sessions or leases.

Restart reasons are typed and observable through worker stats and pool snapshots. The implemented restart policy cycles
by request count, import-like request count, measured RSS ceiling, idle duration, explicit cycle, timeout, cancellation,
and in-flight RSS hard limit.

The current supervisor does not implement an OTP-style restart-intensity window. Do not claim that restarts are limited
by "N restarts per time window" unless that policy is added. The public `RestartLimitExceeded` error variant is reserved
for such a supervising policy; it is not the normal outcome of the current cycling rules.

## Cancellation And Fairness

Parent-side cancellation is cooperative at the worker boundary. The supervisor checks before sending a request and after
progress or data-row frames while a request is in flight. If cancellation is observed after dispatch, the supervisor
cycles the child and returns a typed cancellation error. It does not pre-empt arbitrary Lean execution inside the child.

Fairness is out of scope for the current runtime. There is no scheduler theorem and no guarantee that every possible
piece of work is eventually admitted. Admission depends on caller control flow, pool capacity, RSS policy, worker
liveness, and configured timeouts.

## Formal Model Status

There is no checked Lean transition-system model for the worker runtime today. The implementation should therefore be
described as an operational Rust contract, not as a proved actor calculus. A future formal model should start with
`Config`, `Event`, and a small-step `Step` relation over worker states, pool entries, in-flight requests, stream events,
and restart outcomes, then state how observed Rust traces refine that model.
