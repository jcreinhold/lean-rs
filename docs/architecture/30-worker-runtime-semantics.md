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

Each concrete child process instance has a parent-side generation. The wire protocol does not carry that generation;
the supervisor associates the generation with the in-flight request owner, stdout reader task, and child handle that
produced the frames. A frame from an older generation is classified as stale protocol output before it can satisfy the
current request. Normal serial operation already prevents overlap by dropping old pipes before a replacement is
accepted; the generation check pins that invariant in the parent state.

Restarts are bounded by a moving restart-intensity window. The default policy admits at most 16 restarts per 60 seconds;
callers may tune that with `LeanWorkerRestartPolicy::max_restarts_per_window`. The guard runs after the old child has
reached a terminal state and before a replacement is spawned. If the window is exhausted, the request receives
`RestartLimitExceeded`, the supervisor stops accepting more work, and a caller must create a new worker or pool entry to
start under a fresh policy window.

## Cancellation And Fairness

Parent-side cancellation is cooperative at the worker boundary. The supervisor checks before sending a request and after
progress or data-row frames while a request is in flight. If cancellation is observed after dispatch, the supervisor
cycles the child and returns a typed cancellation error. It does not pre-empt arbitrary Lean execution inside the child.

Fairness is out of scope for the current runtime. There is no scheduler theorem and no guarantee that every possible
piece of work is eventually admitted. Admission depends on caller control flow, pool capacity, RSS policy, worker
liveness, and configured timeouts.

## Shutdown Contract

`lean-rs-worker-parent` owns the worker child lifecycle. Once shutdown starts, the supervisor state moves from
`Running` to `ShuttingDown`, and new request admission returns `LeanWorkerError::ShutdownInProgress`. The current
runtime admits at most one request per worker, so in-flight terminalization means that accepted request receives exactly
one terminal outcome: a terminal `Response`, `Timeout`, `Cancelled`, `RssHardLimitExceeded`, `ChildExited`,
`ChildPanicOrAbort`, or a typed parent-side sink/protocol error.

`LeanWorker::shutdown` is the structured public shutdown path. It sends `Request::Terminate`, waits up to
`LEAN_WORKER_SHUTDOWN_TIMEOUT_DEFAULT` by default, then escalates to `kill` and waits up to
`LEAN_WORKER_KILL_WAIT_TIMEOUT_DEFAULT` for the process to be reaped. The returned `LeanWorkerShutdownReport` records
whether the child was already exited, stopped gracefully, or required kill escalation. `terminate` remains only as a
deprecated compatibility wrapper that returns the final `LeanWorkerExit`.

Policy restarts and explicit cycles use the same cleanup machinery. Idle/request-count/import-count/RSS-ceiling cycles
try graceful shutdown before replacing the child. In-flight timeout, cancellation, and hard-RSS failures use the kill
path because the child may be wedged or already executing a request. Fatal child exit is finalized through the same
wait/reap path.

`Drop for LeanWorker` also invokes the bounded shutdown path. Rust `Drop` cannot return `kill`, protocol, or wait
failures to the caller, so callers that need status must call `shutdown`. Drop still attempts graceful stop, kill
escalation, and wait/reap; it does not silently abandon a child. Abrupt parent process death remains outside Rust
cleanup and must be contained by the host's process manager.

## Runtime Controls And Cleanup Audit

The runtime has two different control boundaries. `lean-rs-host` exposes same-process Lean controls through FFI and Lean
shims. Those controls bound cooperative Lean work, import shape, diagnostic volume, and rendered output size. They do
not provide process reset, hard preemption, or reliable cleanup after a wedged Lean runtime. `lean-rs-worker-parent`
owns worker child lifecycle: spawn, request watchdogs, kill, wait/reap, restart accounting, and lease invalidation.
`lean-rs-worker-child` owns the serial stdio loop inside the child and child-local best-effort exit behavior. Downstream
hosts own admission policy, retry/idempotency policy, semantic cache validity, and server/process-manager lifecycle
above the worker crates.

| Control or cleanup concern | Current implementation location | Current guarantee | Missing guarantee | Owner crate | Downstream host responsibility | Prompt that should implement or verify it |
| --- | --- | --- | --- | --- | --- | --- |
| Heartbeat limits | `lean-rs-host` option bundles; host shims pass `Lean.maxHeartbeats`; Lean runtime checks heartbeats at cooperative check points | Elaboration, kernel-check, module-query, and MetaM paths receive bounded heartbeat budgets | No hard preemption of arbitrary C++/Lean loops; no process reset | `lean-rs-host` | Choose per-operation budgets and route untrusted/long work through workers | Prompt 30 |
| Cooperative cancellation checks | `LeanCancellationToken`; `LeanWorkerCancellationToken`; parent event loop checks after progress/data frames | Cancellation is observed before dispatch and at documented Rust/worker boundaries; in-flight worker cancellation cycles the child | Cannot interrupt an already-running same-process Lean call that does not return to a check point | `lean-rs-host`, `lean-rs-worker-parent` | Wire request/UI/server cancellation into the token and treat cancelled in-flight work as non-committed | Prompts 30 and 31 |
| Diagnostic and row-size limits | `LeanElabOptions`, `LeanMetaOptions`, `ModuleQueryOutputBudgets`, protocol frame cap, bounded worker event buffer | Diagnostics, rendered fields, frames, and parent-side event buffering are bounded; backpressure blocks instead of dropping committed rows | No library-wide semantic commit policy for downstream row payloads | `lean-rs-host`, `lean-rs-worker-protocol`, `lean-rs-worker-parent` | Buffer or commit rows only after terminal success when atomicity matters | Prompt 30; downstream policy |
| Import breadth and cache controls | `LeanSessionImportProfile`; `LeanImportProfileMode`; module snapshot cache and clear request | Full sessions use closed import profiles with `loadExts := true`; profiling can measure import shape; module snapshots are bounded and clearable | Downstream cache validity, cross-run invalidation, and semantic reuse policy are not decided by the worker | `lean-rs-host`, `lean-rs-worker-child` | Own semantic cache keys, source provenance, and result invalidation | Prompt 30; downstream policy |
| Restricted `loadExts := false` import/free-region behavior | `LeanRsHostShims.Environment.bracketedImportQuery`; Lean `withImportModules`; `Environment.freeRegions` | The bracketed lightweight query imports with `loadExts := false`, serializes Rust-owned data, then frees compacted regions | Not safe for normal `LeanSession` or capability workflows with loaded environment extensions | `lean-rs-host` | Use bracketed results only for the restricted read-only query shape | Prompt 30 |
| Normal shutdown | `Request::Terminate`; `LeanWorker::shutdown`; child stdio loop returns after `Response::Terminating` | Explicit shutdown stops admission, asks the child to exit, escalates to kill after a bounded timeout, and waits/reaps | Does not define application/server shutdown order above the worker | `lean-rs-worker-parent`, `lean-rs-worker-child` | Stop accepting service work before dropping handles, and decide service shutdown policy | Prompt 31; downstream policy |
| Dropped worker handles | `Drop for LeanWorker`; `Drop for LeanWorkerSessionLease` | Dropping a live worker runs best-effort bounded shutdown/kill/wait; dropping a lease decrements the active-lease count | `Drop` cannot report wait/kill failure to callers | `lean-rs-worker-parent` | Prefer explicit `shutdown`/cycle paths when exit status matters | Prompt 31 |
| Timeout kill and wait/reap | `read_response_with_events`; `restart_with_reason`; shared shutdown helpers | Request timeout records failure, kills the current child, waits for it, joins the reader, respawns, and reports typed timeout facts | Cannot prove child-side side effects did not happen before the timeout | `lean-rs-worker-parent` | Retry only idempotent or deduplicated operations | Prompt 31 |
| Child crash | EOF/read error classification; `wait_for_exit`; `record_exit_error`; child SIGABRT immediate-exit handler | Fatal child exit becomes typed `ChildPanicOrAbort`/`ChildExited` with captured diagnostics; selected read-only calls can restart and return degraded results | No recovery of the crashed child session; no guarantee that child-side side effects are absent | `lean-rs-worker-parent`, `lean-rs-worker-child` | Treat affected in-flight work as failed or tentative | Prompt 31 |
| Wedged child | Parent request timeout and optional in-flight RSS hard limit | A child that stops producing frames is killed and replaced when the parent deadline or hard RSS guard fires | No same-process recovery if Lean is embedded without the worker boundary | `lean-rs-worker-parent` | Use worker boundary for workloads that can wedge | Prompt 31 |
| In-flight request terminalization | Per-request stdout reader; terminal `Response`; EOF/read error; timeout/cancel/RSS restart paths | One parent request ends with a terminal response, EOF/exit error, timeout, cancellation-triggered cycle, or RSS hard-limit cycle | Terminal success is the only success acknowledgement; partial rows remain tentative | `lean-rs-worker-parent` | Commit rows and downstream effects only after terminal success | Prompt 31 |
| Restart generation | `LeanWorkerLifecycleSnapshot::worker_generation`; private `WorkerGeneration`; in-flight request and reader-task generation tags | Generation bumps on accepted replacement before future work is admitted; parent-side frame consumers know which generation produced each event | No generation field on the wire protocol; external callers observe only snapshots/stats | `lean-rs-worker-parent` | Compare snapshots around operations when supervising higher-level sessions | Prompt 33 |
| Stale output rejection | Serial request model; parent owns stdout during one request; reader events carry parent-side generation; replacement drops old pipes | Normal serial operation cannot interleave two live requests on one child; any stale-generation reader event is rejected as protocol output before request completion | No cross-process request id for future overlapping protocol modes | `lean-rs-worker-parent`, `lean-rs-worker-protocol` | Avoid assuming cross-request output identity beyond the current serial contract | Prompt 33 |
| Restart intensity limits | `LeanWorkerRestartPolicy::max_restarts_per_window`; default 16 restarts per 60 seconds; `LeanWorkerError::RestartLimitExceeded` | Replacement attempts are bounded by a moving window; exhaustion terminalizes the old child, refuses the replacement, and stops accepting more work on that supervisor | No backoff scheduler or automatic recovery after exhaustion | `lean-rs-worker-parent` | Create a new worker/pool entry only after deciding the service-level policy permits another restart window | Prompt 33 |
| Parent/control-channel death | Child stdio loop reads stdin and exits on protocol read error/EOF; child response writes propagate broken-pipe errors; Linux children install `PR_SET_PDEATHSIG(SIGTERM)` | Loss of stdin or stdout causes the child loop to fail and process exit under ordinary pipe semantics; on Linux, parent process death also requests child termination even if stdio remains open | No cross-platform parent-death signal, process-group contract, or guarantee if the OS cannot deliver the best-effort signal | `lean-rs-worker-child` | Use an external supervisor/process manager for service-level parent death policy | Prompt 32 |
| Orphan and zombie prevention | Parent `Drop`, `shutdown`, timeout, cancellation, RSS, and restart paths kill/wait or request terminate/wait | Parent-owned children are reaped on explicit lifecycle paths and best-effort `Drop` | No guarantee if the parent process itself is killed before running cleanup; child-side parent-loss handling is best effort | `lean-rs-worker-parent`, `lean-rs-worker-child` | Configure OS/process-manager containment for abrupt parent death | Prompt 33; downstream policy |

Core child cleanup semantics belong below downstream hosts. Downstream tools such as MCP servers may choose admission,
retry, and service shutdown policy, but they should not become the owner of worker child kill/reap, timeout replacement,
or stale-session invalidation.

## Formal Model Status

There is no checked Lean transition-system model for the worker runtime today. The implementation should therefore be
described as an operational Rust contract, not as a proved actor calculus. A future formal model should start with
`Config`, `Event`, and a small-step `Step` relation over worker states, pool entries, in-flight requests, stream events,
and restart outcomes, then state how observed Rust traces refine that model.
