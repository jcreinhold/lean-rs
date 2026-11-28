# Worker Runtime Semantics

This is the canonical model for `lean-rs-worker-protocol`, `lean-rs-worker-parent`, and `lean-rs-worker-child`. The
worker runtime is a resource-bounded supervised process service with affine leases, generation-indexed requests,
parent-owned cleanup, restart intensity limits, and bounded streaming buffers.

This prose contract guides worker, pool, session, and protocol changes. Preserve its stable labels when changing the
runtime contract. Named Rust conformance tests supply selected implementation evidence, with the limits described in
[Proof status](#proof-status).

## Glossary

**Service.** One child process plus the protocol endpoint it serves.

**Supervisor.** The parent-side controller for one service. In code this is `LeanWorker`.

**Request.** One framed protocol command admitted by a supervisor, with at most one parent-visible terminal outcome.

**Generation.** A parent-side epoch identifying one concrete child process instance. The wire protocol does not carry
the generation.

**Lease.** An affine right returned by the pool to use one admitted service resource. In code this is
`LeanWorkerSessionLease`.

**Admission.** The pool transition that grants, refuses, or synchronously waits for a lease request.

**Trace.** A finite sequence of observable runtime events after hiding log-only, metric-only, and scheduling-noise
events.

**Commit.** The downstream decision to treat delivered rows as durable after terminal success. The worker transports
rows but does not define downstream row semantics.

## Not An Actor Model

The worker runtime deliberately omits actor-model obligations. There are no public actor identities, cloneable actor
addresses, mailboxes, behavior-update rules, delivery policy, scheduler theorem, or fairness guarantee. A pool admission
wait is a bounded synchronous wait at a call site, not a reserved mailbox slot. `LeanWorkerPoolSnapshot` has a stable
`queue_depth` field, but that field is currently `0` and is not evidence of a request queue.

The public worker contract is a serial request/response supervisor and a local affine lease pool.

## Mathematical Objects

Let:

- `G` be the natural-number set of worker generations.
- `Req` be protocol requests, including health, session open, typed commands, streaming commands, shutdown, and test
  harness requests.
- `Resp` be terminal protocol responses.
- `Row` be stream rows `(stream, sequence, payload)`.
- `Diag` and `Prog` be worker diagnostics and progress events.
- `Fail` be parent-visible failures: protocol error, child exit, child panic or abort, timeout, cancellation, RSS
  hard-limit kill, sink panic, restart-limit exhaustion, pool admission refusal, wait failure, kill failure, and
  shutdown-in-progress.
- `Res` be resource facts: worker capacity, known child RSS, per-frame byte cap, event-buffer capacity, request timeout,
  shutdown timeout, kill-wait timeout, restart window, import-like request counters, and cancellation state.
- `Client` be pool clients that request leases for `LeanWorkerSessionKey` values.

The observable events include:

```text
spawn(g)                  accept(g, r)
row(g, r, b)              diagnostic(g, r, d)
progress(g, r, p)         terminal(g, r, o)
shutdown_start(g)         terminate_sent(g)
kill_sent(g)              reaped(g, exit)
restart_admitted(g, g')   restart_refused(g, reason)
lease_granted(c, l)       lease_released(l)
lease_dropped(l)          admission_refused(c, reason)
```

where `o in Resp + Fail` and `b` ranges over finite row buffers.

The intended refinement mapping hides child pids, OS pipe handles, stderr fragments not surfaced in typed errors,
tracing spans, metrics-only counters, exact thread scheduling, and handshake/configuration frame order. This mapping is
not an implementation-trace refinement theorem.

## Conformance Trace Harness

The implementation-level conformance harness lives in `crates/lean-rs-worker-parent/tests/worker_shutdown.rs`. Its
`RuntimeTraceEvent` enum is test-only vocabulary, not a public tracing API and not an IPC message. The enum names the
parent-observable events that later supervisor and pool refactors must preserve:

| Test trace event | Model event or invariant |
| --- | --- |
| `GenerationStarted(g)` | worker generation `g` exists after spawn |
| `RequestAdmitted { generation: g, request: r }` | `accept(g, r)` admission |
| `RequestSent { generation: g, request: r }` | transition into `Busy(g, r)` |
| `StreamRowObserved { generation: g, request: r, .. }` | `row(g, r, b)` before commit |
| `BackpressureObserved { generation: g, request: r, waits }` | bounded parent event buffer applied backpressure before terminal outcome |
| `TerminalOutcomeObserved { generation: g, request: r, outcome }` | `terminal(g, r, o)` and terminal outcome uniqueness |
| `TimeoutObserved { generation: g, request: r }` | timeout failure before kill/reap |
| `ChildCrashObserved { generation: g, request: r }` | `Crashed(g, c)` before terminalization |
| `RestartObserved { from: g, to: g' }` | `restart_admitted(g, g')` and generation separation |
| `RestartLimitExhausted { generation: g }` | `restart_refused(g, restart_limit_exceeded)` |
| `ShutdownStarted { generation: g }` | `shutdown_start(g)` |
| `GracefulStopAttempted { generation: g }` | `terminate_sent(g)` |
| `KillEscalated { generation: g }` | `kill_sent(g)` |
| `ChildReaped { generation: g }` | `reaped(g, exit)` |
| `LeaseGranted { .. }` | `lease_granted(c, l)` via public pool snapshots |
| `LeaseReleased { .. }` | `lease_released(l)` via public pool snapshots |
| `LeaseDropped { .. }` | `lease_dropped(l)` via public pool snapshots |
| `IdleReplacementObserved { .. }` | idle policy replacement preserves one live generation and one accounting slot |
| `AdmissionRefused { reason }` | `admission_refused(c, reason)` |

The conformance tests are import-light. The test binary re-enters itself as a deterministic fake worker child, and the
pool tests use a valid minimal manifest plus an executable wrapper rather than adding a generic child environment
passthrough. Real Lean import behavior remains covered by worker-child and isolated nextest suites.

Preserve or extend these named conformance tests when refactoring their behavior:

- `conformance_terminal_success_has_one_terminal_outcome`;
- `conformance_name_rows_complete_with_one_terminal_outcome`;
- `conformance_name_rows_then_child_exit_fail_the_request`;
- `conformance_name_rows_then_child_abort_fail_the_request`;
- `conformance_name_rows_then_silence_time_out`;
- `conformance_silent_worker_cancellation_reaps_only_owned_child`;
- `conformance_silent_worker_deadline_is_not_renewed_by_polling`;
- `conformance_slow_progress_sink_backpressure_is_bounded_and_observable`;
- `conformance_explicit_shutdown_gracefully_reaps_child`;
- `conformance_dropped_idle_worker_reaps_child`;
- `conformance_dropped_worker_escalates_kill_and_reaps_child`;
- `conformance_timeout_kill_reap_restarts_next_generation`;
- `conformance_child_crash_terminalizes_in_flight_request`;
- `conformance_restart_limit_exhaustion_is_typed_terminal_outcome`;
- `planned_restarts_do_not_use_the_restart_window`;
- `crashed_child_is_replaced_and_counted`;
- `exhausted_worker_starts_again_after_the_window`;
- `expired_deadline_leaves_the_replacement_to_the_next_request`;
- `stopped_worker_refuses_requests_until_cycle`;
- `missing_session_is_opened_again_in_a_new_child`;
- `conformance_stale_generation_output_is_protocol_failure`.

The stale-generation test is a unit test in `supervisor.rs`; the other names above belong to the worker-shutdown
integration harness. These tests provide selected implementation evidence, not a mechanized trace-refinement proof.

## Worker Transition System

A worker state has one of these forms:

```text
Absent
Starting
Idle(g)
Busy(g, r)
Streaming(g, r, b)
Stopping(g)
Killing(g)
Reaping(g)
Crashed(g, c)
Exited(g)
RestartExhausted(g, t)
```

`g in G`, `r in Req`, `b` is the finite sequence of rows and progress/diagnostic events already delivered for the
current request, `c` is an observed child exit or fatal condition, and `t` is the time the oldest counted restart leaves
the restart window. The current implementation keeps process handles, stdio pipes, and stats as separate fields, but its
private `WorkerSupervisorState` records the model-facing phases: idle, busy, streaming, stopping, killing, reaping,
crashed, restart-exhausted, and exited. Spawn-time `Starting` remains inside `LeanWorker::spawn` rather than as a stored
state on a constructed supervisor. An armed startup guard owns the child through pipe setup, handshake, and frame-limit
negotiation. Any early error before ownership transfers to the supervisor kills and polls for reaping of that child
within the normal kill-wait deadline. Successful startup disarms the guard without disturbing the child.

The core transitions are:

- `Absent -> Starting -> Idle(g)` when the supervisor spawns a child and completes the protocol handshake.
- `Idle(g) -> Busy(g, r)` when the parent admits and writes request `r`.
- `Busy(g, r) -> Streaming(g, r, b)` when the child emits a non-terminal row, diagnostic, or progress event.
- `Streaming(g, r, b) -> Streaming(g, r, b ++ [e])` for additional non-terminal events.
- `Busy(g, r)` or `Streaming(g, r, b) -> Idle(g)` when a terminal success response is accepted.
- `Busy(g, r)` or `Streaming(g, r, b) -> Killing(g) -> Reaping(g)` on timeout, in-flight RSS hard limit, or cancellation
  observed after dispatch.
- `Idle(g) -> Stopping(g) -> Reaping(g)` on explicit shutdown, graceful policy cycle, request-count cycle, import-count
  cycle, idle cycle, or RSS-ceiling cycle.
- Any state with a live child may move to `Crashed(g, c) -> Reaping(g)` when the child exits or the protocol stream ends
  unexpectedly.
- `Reaping(g) -> Starting -> Idle(g + 1)` when the supervisor admits the replacement, at once after a crash, timeout,
  cancellation, hard RSS kill, protocol failure, or policy cycle.
- `Starting -> Exited(g + 1)` when an admitted replacement fails to start.
- `Reaping(g) -> Exited(g)` on explicit shutdown, or when a request's own deadline has passed, since no replacement can
  start inside it.
- `Exited(g) -> Starting -> Idle(g + 1)` on the next request or `cycle`, which records why the old child went. After
  explicit shutdown only `cycle` takes this step; requests are refused.
- `Reaping(g)` or `Exited(g) -> RestartExhausted(g, t)` when the restart window refuses an abnormal replacement, and
  `RestartExhausted(g, t) -> Starting -> Idle(g + 1)` on the first request or `cycle` after `t`. Before `t` both are
  refused with `RestartLimitExceeded`.

## Pool Transition System

A pool state is a tuple:

```text
(capacity, entries, leases, waiting, shutting_down, restart_budget, memory_facts)
```

`capacity` is `LeanWorkerPoolConfig::max_workers`. `entries` is the finite set of local worker entries keyed by
`LeanWorkerSessionKey`. `leases` maps live lease tokens to entries. `waiting` is either empty or a caller currently
blocked in synchronous admission; it is not a durable queue. `shutting_down` records whether the pool is being dropped
or no longer admits work. `restart_budget` is the per-worker restart-intensity window. `memory_facts` contains known RSS
samples and configured total/per-worker RSS ceilings, with an explicit unavailable state on platforms where RSS cannot
be sampled.

Pool admission has three outcomes:

- Grant a lease for a compatible warm entry.
- Spawn or cycle a local entry and grant a lease if capacity and memory facts permit it.
- Refuse or time out with `WorkerPoolExhausted`, `WorkerPoolMemoryBudgetExceeded`, or `WorkerPoolQueueTimeout`.

The pool never publishes child ids, pids, pipes, selected-entry identity, frame order, or a global FIFO order.

## Safety Invariants

**Terminal outcome uniqueness.** For every admitted pair `(g, r)`, the parent accepts at most one terminal outcome:
`terminal(g, r, o)`. Terminal outcomes include terminal protocol responses and typed parent-side failures. A successful
write to stdin is not a success acknowledgement; only a terminal success response is.

**Request and generation separation.** A response or row observed from generation `g` may satisfy only a request
admitted under generation `g`. The current serial protocol has no request id on the wire, so the parent tags reader
events with the private in-flight request id before delivering progress, diagnostics, rows, or terminal responses. If
the parent-side reader reports generation `g' != g` or a request id that is no longer current, the event is stale
protocol output and cannot complete the current request.

**Affine lease law.** A lease can be consumed, released, or dropped at most once. After release or invalidation, the old
lease grants no authority over later generations or replacement children.

**Serial request law.** One supervisor admits at most one parent request at a time. There is no overlapping request id
space in the current protocol.

**Bounded admission counters.** Pool capacity is bounded by `max_workers`. Frame size is bounded by the negotiated
per-frame cap. Streaming parent buffering is bounded by the internal event-buffer capacity. These are admission and
buffering facts, not claims that every OS allocation is capped.

**Parent-owned shutdown.** `lean-rs-worker-parent` owns the cleanup primitive: stop admission, terminalize accepted
in-flight work, attempt graceful termination when appropriate, escalate to kill when needed, wait for the child, and
record a structured result.

**Cleanup terminalization.** Explicit shutdown, policy cycle, timeout, cancellation-triggered cycle, hard-RSS kill,
fatal exit, and `Drop` all drive the child toward a terminal reaped state as far as the parent process is alive and Rust
cleanup can run. For requests carrying a cancellation token, the IPC wait checks that token at intervals of at most 50
ms even when the worker emits no events. This does not renew the request deadline, and excludes caller sink execution
and OS scheduling delays. The cancellation result follows child kill/reap, reader drain, and replacement; when the
request's own deadline has passed, the next request or `cycle` starts the replacement instead.

**Restart intensity.** Each restart reason is planned or abnormal, as `LeanWorkerRestartReason::is_planned` reports. A
planned restart carries out the restart policy or an explicit cycle on a working child; the window neither counts nor
refuses it. An abnormal restart replaces a child that failed, and a moving window bounds those: the default admits 16
per 60 seconds. A refused replacement returns `RestartLimitExceeded` with the time until the oldest counted restart
leaves the window. Until then the worker has no child, requests and `cycle` fail with the same error, and
`lifecycle_snapshot().restart_retry_after` reports the wait. After it, the next request starts a child.

**Streaming commit.** Rows are visible to the sink immediately. Rows delivered before terminal success are tentative.
Cancellation, timeout, fatal exit, stale request or generation, row-decode failure, sink panic, or shutdown failure
prevents the worker from claiming that those rows are committed. Downstream callers own commit, deduplication, and cache
validity.

**No stale rows after reset.** Rows from an old generation are not allowed to satisfy a request in a replacement
generation. A replacement drops old pipes, joins the old reader path, and tags parent-side reader events by generation.

## Lifecycle Semantics

**Request lifecycle.** A request begins only after policy checks and successful parent-side admission. The child
processes framed requests serially from stdin. A terminal response, EOF/fatal child exit, timeout, cancellation, RSS
hard-limit kill, protocol error, sink panic, or shutdown path ends the request at the parent boundary.

**Shutdown semantics.** `LeanWorker::shutdown` is the structured public shutdown path. It sends `Request::Terminate`,
waits up to `LEAN_WORKER_SHUTDOWN_TIMEOUT_DEFAULT` by default, escalates to `kill`, then waits up to
`LEAN_WORKER_KILL_WAIT_TIMEOUT_DEFAULT` for the process to be reaped. The returned `LeanWorkerShutdownReport` records
whether the child had already exited, stopped gracefully, or required kill escalation. `terminate` remains a deprecated
compatibility wrapper over this structured path.

**Failed replacement startup.** A reaped generation's exit does not prove that a subsequently spawned replacement has
exited. Every replacement path uses the same tracked startup: if setup fails and bounded cleanup cannot confirm exit,
the supervisor retains that replacement's process handle. Status fails rather than treating the old exit as current
proof, and no request runs on the retained process. The next request or `cycle` retries bounded cleanup and starts a new
child only after the retained one is reaped. Structured shutdown retries the same cleanup and reports success only after
the process is reaped. A failure before spawn, or setup failure followed by confirmed cleanup, leaves no unconfirmed
child and can still report an already-exited service. This applies to explicit, idle, memory-policy, cancellation,
timeout, and crash-triggered replacement; it is independent of the host's capacity policy.

**Request-bound stop evidence.** Take `request_checkpoint()` immediately before a synchronous worker operation and query
`confirmed_request_stop_since(&checkpoint)` afterward. A receipt identifies the latest attempted protocol request, its
operation, generation, direct-child PID and confirmed exit. The opaque checkpoint belongs to one supervisor and survives
its replacements; another supervisor cannot use it. Attempts are identified before the first request write, so a
partially written frame is covered. A newer attempt without a stop receipt shadows any older receipt. Request and
generation counters reject exhaustion rather than reuse an identity.

The receipt survives successful replacement and errors after the original child's confirmed exit. It is separate from
`lifecycle_snapshot().last_exit`, which describes the current lifecycle state and may be cleared by replacement. Cleanup
of a failed replacement does not change the original request receipt. The receipt proves only that this request cannot
continue in its named direct child. It proves neither a usable result nor absence of earlier effects, descendant exit,
replacement exit, retry safety, or releasable pool capacity. A host persists its own server/actor identity alongside
receipt fields; the in-process checkpoint is not a durable process handle.

**Drop semantics.** `Drop for LeanWorker` invokes the same bounded shutdown machinery. Rust `Drop` cannot return
protocol, kill, or wait failures, so callers needing status must call `shutdown`. Drop is a cleanup obligation, not a
status-reporting API.

**Parent-death handling.** On Unix each child holds the read end of a lifeline pipe and exits as soon as its parent
dies, even while a request is inside Lean.
[Parent death and inherited descriptors](#parent-death-and-inherited-descriptors) states the mechanism and its limits.

**Restart semantics.** Crash, timeout, cancellation after dispatch, shutdown, explicit cycle, memory cycling, and
restart-limit exhaustion all end the current generation. Each admitted restart creates generation `g + 1`, even when its
child then fails to start, so the generation always equals the restart count; a refused restart creates none. Future
work must be admitted in the new generation.

## Controls Are Not Containment

Same-process controls live in `lean-rs-host`: heartbeat budgets, cooperative cancellation checks, output budgets,
import-profile selection, module snapshot cache clearing, and the restricted read-only `loadExts := false` /
`Environment.freeRegions` query path. They bound cooperative Lean work or scoped cache use. They cannot provide hard
preemption, full Lean runtime reset, cleanup of a wedged runtime, or recovery after a fatal Lean abort.

Subprocess containment lives in the worker crates. `lean-rs-worker-parent` owns spawn, request watchdogs, kill,
wait/reap, restart accounting, generation separation, and lease invalidation. `lean-rs-worker-child` owns the serial
stdio loop and best-effort child-local exit behavior. ABI/FFI access is not a substitute for this process boundary.

## Downstream Host Policy

Downstream hosts own when to acquire leases, when to call shutdown, whether to retry after an ambiguous failure, how to
evict pools, how to commit or discard rows, and how to interpret domain schemas. They may choose service-level
supervision, but they do not own the primitive child cleanup semantics inside `lean-rs`. Core kill/reap, timeout
replacement, restart intensity, and stale-session invalidation remain in `lean-rs-worker-parent` and
`lean-rs-worker-child`.

An application that selects native images can opt in to `checked_native_loader_environment` on its worker configuration.
Each launch and restart rejects loader overrides before it starts a child. The application still owns executable and
image checks. Use `inherit_checked_native_loader_environment` for a separate helper only when the helper should inherit
the parent environment; it freezes that environment with the helper's explicit additions and removals.

## Operational Assumptions

- OS process signaling, pipe EOF, broken-pipe behavior, and RSS sampling follow the platform contracts available to Rust
  and libc on the target system.
- Rust threads and synchronization eventually schedule parent cleanup while the parent process remains alive.
- Parent `Drop` runs only during ordinary Rust destruction; abrupt parent process death may skip it.
- External memory accounting is approximate. RSS limits are policy decisions over samples, not a proof that every byte
  is reclaimed before the OS reaps a process.
- Request timeouts and cancellation are parent-side control decisions. They do not prove that no child-side side effect
  occurred before the child was killed.

## Conditional Liveness

The model makes only conditional liveness claims:

- If a child responds before the request timeout, the parent reader remains alive, the row/diagnostic/progress sinks do
  not panic, cancellation is not requested, and the child generation matches the admitted request, the request reaches a
  terminal parent-visible outcome.
- If the parent process remains alive and OS kill/wait operations work, explicit shutdown and parent-owned lifecycle
  paths eventually reach a reaped child or return a structured wait/kill failure.
- If pool capacity and memory facts permit admission, and no compatible warm lease is held indefinitely by another
  caller, a synchronous acquisition may return a lease before `queue_wait_timeout`.

There is no unconditional fairness or eventual-admission theorem.

## Implementation ownership

- `supervisor.rs` must implement `Worker transition system`, `Terminal outcome uniqueness`, `Generation separation`,
  `Parent-owned shutdown`, `Restart intensity`, and `Parent-death handling` as parent-observed lifecycle clauses.
- `session.rs` must preserve the process-safe session subset, cancellation boundary, streaming row tentativeness, and
  affine sink/lease behavior exposed through worker sessions.
- `pool.rs` must implement `Pool transition system`, `Affine lease law`, bounded admission, memory-policy refusal, and
  lease invalidation after lifecycle transitions.
- `lean-rs-worker-child` must implement the serial child request loop, terminal protocol responses, portable pipe-loss
  exit, and platform-specific parent-death hardening where available.
- `lean-rs-worker-protocol` must keep request, response, diagnostic, progress, row, stream-summary, and fatal-exit types
  compatible with the serial request model unless the runtime contract is extended first.

## Runtime Controls And Cleanup Audit

The table maps the control and cleanup contracts above to their implementation owners.

| Control or cleanup concern | Current implementation location | Current guarantee | Missing guarantee | Owner crate | Downstream host responsibility |
| --- | --- | --- | --- | --- | --- |
| Heartbeat limits | `lean-rs-host` option bundles; host shims pass `Lean.maxHeartbeats`; Lean runtime checks heartbeats at cooperative check points | Elaboration, kernel-check, module-query, and MetaM paths receive bounded heartbeat budgets | No hard preemption of arbitrary C++/Lean loops; no process reset | `lean-rs-host` | Choose per-operation budgets and route untrusted/long work through workers |
| Cooperative cancellation checks | `LeanCancellationToken`; `LeanWorkerCancellationToken`; parent event loop checks after progress/data frames | Cancellation is observed before dispatch and at documented Rust/worker boundaries; in-flight worker cancellation cycles the child | Cannot interrupt an already-running same-process Lean call that does not return to a check point | `lean-rs-host`, `lean-rs-worker-parent` | Wire request/UI/server cancellation into the token and treat cancelled in-flight work as non-committed |
| Diagnostic and row-size limits | `LeanElabOptions`, `LeanMetaOptions`, `ModuleQueryOutputBudgets`, protocol frame cap, bounded worker event buffer | Diagnostics, rendered fields, frames, and parent-side event buffering are bounded; backpressure blocks instead of dropping committed rows | No library-wide semantic commit policy for downstream row payloads | `lean-rs-host`, `lean-rs-worker-protocol`, `lean-rs-worker-parent` | Buffer or commit rows only after terminal success when atomicity matters |
| Import breadth and cache controls | `LeanSessionImportProfile`; `LeanImportProfileMode`; module snapshot cache and clear request | Full sessions use closed import profiles with `loadExts := true`; profiling can measure import shape; module snapshots are bounded and clearable | Downstream cache validity, cross-run invalidation, and semantic reuse policy are not decided by the worker | `lean-rs-host`, `lean-rs-worker-child` | Own semantic cache keys, source provenance, and result invalidation |
| Restricted `loadExts := false` import/free-region behavior | `LeanRsHostShims.Environment.bracketedImportQuery`; Lean `withImportModules`; `Environment.freeRegions` | The bracketed lightweight query imports with `loadExts := false`, serializes Rust-owned data, then frees compacted regions | Not safe for normal `LeanSession` or capability workflows with loaded environment extensions | `lean-rs-host` | Use bracketed results only for the restricted read-only query shape |
| Normal shutdown | `Request::Terminate`; `LeanWorker::shutdown`; child stdio loop returns after `Response::Terminating` | Explicit shutdown stops admission, asks the child to exit, escalates to kill after a bounded timeout, and waits/reaps | Does not define application/server shutdown order above the worker | `lean-rs-worker-parent`, `lean-rs-worker-child` | Stop accepting service work before dropping handles, and decide service shutdown policy |
| Dropped worker handles | `Drop for LeanWorker`; `Drop for LeanWorkerSessionLease` | Dropping a live worker runs best-effort bounded shutdown/kill/wait; dropping a lease decrements the active-lease count | `Drop` cannot report wait/kill failure to callers | `lean-rs-worker-parent` | Prefer explicit `shutdown`/cycle paths when exit status matters |
| Timeout kill and wait/reap | `read_response_with_events`; `restart_with_reason`; shared shutdown helpers | Request timeout records failure, kills the current child, waits for it, joins the reader, and reports typed timeout facts; the child is replaced at once, or by the next request or `cycle` when the request's own deadline has passed | Cannot prove child-side side effects did not happen before the timeout | `lean-rs-worker-parent` | Retry only idempotent or deduplicated operations |
| Child crash | EOF/read error classification; `wait_for_exit`; `record_exit_error`; child SIGABRT immediate-exit handler | Fatal child exit fails the request with typed `ChildPanicOrAbort`/`ChildExited` and captured diagnostics, and the supervisor replaces the child, recording `ChildAbort` or `ChildExit`; a child found dead between requests is replaced by the next request; selected read-only calls return degraded results instead of the error | No recovery of the crashed child session; no guarantee that child-side side effects are absent | `lean-rs-worker-parent`, `lean-rs-worker-child` | Treat affected in-flight work as failed or tentative |
| Wedged child | Parent request timeout and optional in-flight RSS hard limit | A child that stops producing frames is killed and replaced when the parent deadline or hard RSS guard fires | No same-process recovery if Lean is embedded without the worker boundary | `lean-rs-worker-parent` | Use worker boundary for workloads that can wedge |
| In-flight request terminalization | Per-request stdout reader; terminal `Response`; EOF/read error; timeout/cancel/RSS restart paths | One parent request ends with a terminal response, EOF/exit error, timeout, cancellation-triggered cycle, or RSS hard-limit cycle | Terminal success is the only success acknowledgement; partial rows remain tentative | `lean-rs-worker-parent` | Commit rows and downstream effects only after terminal success |
| Restart generation | `LeanWorkerLifecycleSnapshot::worker_generation`; private `WorkerGeneration`; in-flight request and reader-task generation tags | Generation bumps on every admitted restart before future work is admitted, so it equals the restart count; parent-side frame consumers know which generation produced each event | No generation field on the wire protocol; snapshots and request-bound stop evidence remain parent-side observations | `lean-rs-worker-parent` | Compare snapshots around operations when supervising higher-level sessions |
| Stale output rejection | Serial request model; parent owns stdout during one request; reader events carry parent-side generation; replacement drops old pipes | Normal serial operation cannot interleave two live requests on one child; any stale-generation reader event is rejected as protocol output before request completion | No cross-process request id for future overlapping protocol modes | `lean-rs-worker-parent`, `lean-rs-worker-protocol` | Avoid assuming cross-request output identity beyond the current serial contract |
| Restart intensity limits | `LeanWorkerRestartPolicy::max_restarts_per_window`; default 16 abnormal restarts per 60 seconds; `LeanWorkerError::RestartLimitExceeded`; `LeanWorkerLifecycleSnapshot::restart_retry_after` | Abnormal replacements are bounded by a moving window and planned ones are not counted; a refused replacement leaves no child until the oldest counted restart leaves the window, and then the next request starts one | No backoff beyond the window | `lean-rs-worker-parent` | Report the wait, and decide whether a call waits for it or fails |
| Parent/control-channel death | Unix lifeline pipe watched by a child thread that calls `_exit(125)` at end of file; child stdio loop exits on stdin EOF or a failed stdout write | A child exits promptly when its parent dies, even while a request is inside Lean | The lifeline ends only the worker child, not processes the child starts; see the limits below | `lean-rs-worker-parent`, `lean-rs-worker-child` | Use an external supervisor/process manager for service-level parent death policy |
| Orphan and zombie prevention | Parent `Drop`, `shutdown`, timeout, cancellation, RSS, and restart paths kill/wait or request terminate/wait; the lifeline covers abrupt parent death | Parent-owned children are reaped on explicit lifecycle paths and best-effort `Drop`; a child whose parent died exits and is reaped by its new parent | Ordinary Drop may be skipped on abrupt parent death | `lean-rs-worker-parent`, `lean-rs-worker-child` | Stop processes the worker starts through their own process group or supervisor |

## Proof Status

This document states the SDK worker and pool contract. Its named Rust conformance tests exercise selected implementation
behaviors; they do not establish trace refinement or shutdown liveness. No formal model checks this contract.

## Worker pipe I/O (macOS and Linux)

The parent uses nonblocking Unix pipe ends. Read and write loops check cancellation or deadlines between attempts; they
wait 5 ms when a pipe is not ready. The child keeps blocking pipe ends. These changes do not claim Windows support.

Stderr drains from startup into a 64 KiB byte tail. After exit, collection allows 100 ms for EOF, then stops and joins
its reader even if a descendant still holds the pipe open. Stdout readers also receive a stop signal before joining. No
pipe thread needs a descendant to close an inherited descriptor before it can stop.

Request deadlines include writing stdin and reading the response. The parent checks expiry again after dequeuing a
frame, before accepting it. Cancellation, timeout, or a write I/O error kills and reaps the child: a possibly partial
frame is never reused. Encoding and frame-size errors before writing do not retire a healthy worker. Sending the
shutdown request also has a deadline. Stderr is diagnostic output, never proof evidence.

These deadlines bound pipe waits, not arbitrary synchronous caller callbacks or serialization. Killing, reaping, and
starting a replacement also take time after a request expires. If the OS cannot confirm exit, cleanup returns an error
and keeps ownership rather than treating the child as stopped. The process tracker below keeps that child counted until
cleanup confirms its owned scope is closed.

## Confirmed child exit and capacity

`LeanWorkerProcessTracker` counts child processes across startup and replacement. Pass one shared tracker to every
worker builder whose children an embedder counts against one limit. After the owner stops and cannot spawn again, it may
release that capacity when `active_processes()` returns zero. The tracker reserves one count before spawn, after it
starts a cleanup thread, and a failed spawn returns the reservation. After spawn, only a confirmed direct exit releases
the count.

`try_shutdown(&mut self)` allows cleanup retries without consuming the owner. Dropping an owner moves any unconfirmed
child into the tracker's cleanup registry. The reaper remains available while launches or children are outstanding,
wakes on transfer and retries unresolved handles every 100 ms. If the thread cannot start, launch fails before spawn.
Cleanup sends a kill only after `try_wait` reports the owned child still running; a wait error never authorizes a signal
to a reused PID. Other wait errors, including `ECHILD`, keep the actual child and its count, so a child that some other
wait call reaped stays counted.

## Source-header imports and retained memory

The worker protocol reports cumulative successful source-header import bytes before each source response. The child
excludes memory-mapped regions and regions already charged to the input session. The parent adds only increases to its
generation budget. Evicting a snapshot, a reusable header, or a session never subtracts this charge: these operations do
not free Lean's imported regions.

Source headers have a separate bounded cache. The RSS snapshot guard can discard large elaboration snapshots without
forcing another header import. Matching warm headers and sessions remain usable after the byte budget is reached. A
fresh generation may perform its first necessary session and header import even if they exceed that budget. A later
actual cache miss is declined before importing; the parent recycles, reopens the session, and retries once. A second
recycle response fails rather than looping.

A failed header import can leave allocations whose size is unknown. Its original result and diagnostics are returned
without retry. Before the next request, the parent replaces the child and reopens the required session. A missing
accounting response due to a child crash remains a failed request under the ordinary process-failure rules, never a
pass. This byte budget is not a peak-memory prediction or a substitute for process isolation.

The direct in-process host API guards imports, source elaboration, and cache/accounting access with one process-local
mutex because Lean's extension registry and these caches are global. Calls wait up to 30 seconds for another thread to
finish; timeout returns an explicit resource error before entering Lean. Same-thread recursive entry fails immediately,
so a synchronous callback cannot deadlock on its own lock. A callback that starts another thread and waits for that
thread can still encounter the bounded timeout; callers should not synchronously reenter the host that way. Rust
unwinding releases both the lock and the thread-local recursion guard, including after a panic. Ordinary lookups and
separate worker processes do not acquire this mutex. Normal concurrent direct-host imports retain their waiting
behavior; stuck contention no longer waits indefinitely.

`max_imports` keeps its session-import count meaning (plus the existing import-like fixture calls). The child checks
that count on actual session-cache misses, even when the parent predicted reuse. Source-header imports contribute to the
retained-byte budget, not this session-count limit. With the byte budget disabled, header churn has no count limit; use
the byte budget to bound it. Header import counts remain available through host accounting and protocol events.

## Parent death and inherited descriptors

On Unix the parent creates a pipe for each child it starts and keeps the write end. The child inherits the read end and
finds its number in `LEAN_RS_WORKER_PARENT_LIFELINE`. Before the Lean runtime starts, a child thread checks that the
descriptor is the read end of a pipe, marks it close-on-exec so that processes the child starts do not hold it, and
blocks reading it. When the parent exits or drops the worker, the kernel closes the write end, the read returns end of
file, and the thread calls `_exit(125)`. The child therefore exits even while its main thread is inside a Lean call, and
it runs no destructors on the way out. A child started without the variable relies on stdin EOF.

Each child runs in its own process group (`process_group(0)`), apart from the parent's terminal and its other children.
The lifeline ends only the worker child; processes it starts keep running until their own owner stops them.

Children inherit every descriptor the parent leaves open without `FD_CLOEXEC`, and the SDK never closes a descriptor it
did not open.

macOS has no atomic close-on-exec pipe call: the standard library calls `pipe` and then sets `FD_CLOEXEC`. The SDK
serializes that step with its other worker spawns, but a thread outside the SDK that starts a process in that window,
such as a Lake build, gives that process a copy of the write end. The worker then outlives its parent until that process
exits. Linux creates the pipe with `pipe2(O_CLOEXEC)` and has no such window.
