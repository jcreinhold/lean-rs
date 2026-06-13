# Worker Runtime Semantics

This is the canonical model for `lean-rs-worker-protocol`, `lean-rs-worker-parent`, and
`lean-rs-worker-child`. The worker runtime is a resource-bounded supervised process service with affine leases,
generation-indexed requests, parent-owned cleanup, restart intensity limits, and bounded streaming buffers.

The model is prose mathematics for implementers. It is not a mechanized proof, and later implementation prompts should
cite the stable labels in this document when they change worker, pool, session, or protocol behavior.

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
addresses, mailboxes, behavior-update rules, delivery policy, scheduler theorem, or fairness guarantee. A pool
admission wait is a bounded synchronous wait at a call site, not a reserved mailbox slot. `LeanWorkerPoolSnapshot` has a
stable `queue_depth` field, but that field is currently `0` and is not evidence of a request queue.

The public contract is instead a serial request/response process supervisor and a local affine lease pool. Actor
terminology should appear in this repository only as this negative comparison unless the implementation grows the
missing actor obligations and states their delivery and fairness policy.

## Mathematical Objects

Let:

- `G` be the natural-number set of worker generations.
- `Req` be protocol requests, including health, session open, typed commands, streaming commands, shutdown, and test
  harness requests.
- `Resp` be terminal protocol responses.
- `Row` be stream rows `(stream, sequence, payload)`.
- `Diag` and `Prog` be worker diagnostics and progress events.
- `Fail` be parent-visible failures: protocol error, child exit, child panic or abort, timeout, cancellation,
  RSS hard-limit kill, sink panic, restart-limit exhaustion, pool admission refusal, wait failure, kill failure, and
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

Implementation traces refine this model by hiding child pids, OS pipe handles, stderr fragments that are not surfaced
in typed errors, tracing spans, metrics-only counters, exact thread scheduling, and the private frame order of
handshake/configuration setup.

## Conformance Trace Harness

Prompt 38 adds a narrow implementation-level conformance harness in
`crates/lean-rs-worker-parent/tests/worker_shutdown.rs`. Its `RuntimeTraceEvent` enum is test-only vocabulary, not a
public tracing API and not an IPC message. The enum names the parent-observable events that later supervisor and pool
refactors must preserve:

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

Prompts 39 through 41 should preserve or extend these exact test names:

- `conformance_terminal_success_has_one_terminal_outcome`;
- `conformance_stream_rows_are_tentative_until_terminal_success`;
- `conformance_stream_child_exit_after_rows_discards_tentative_rows`;
- `conformance_stream_child_crash_after_rows_discards_tentative_rows`;
- `conformance_stream_timeout_after_rows_discards_tentative_rows`;
- `conformance_stream_cancellation_after_rows_discards_tentative_rows`;
- `conformance_stream_backpressure_is_bounded_and_observable`;
- `conformance_explicit_shutdown_gracefully_reaps_child`;
- `conformance_dropped_idle_worker_reaps_child`;
- `conformance_dropped_worker_escalates_kill_and_reaps_child`;
- `conformance_timeout_kill_reap_restarts_next_generation`;
- `conformance_child_crash_terminalizes_in_flight_request`;
- `conformance_restart_limit_exhaustion_is_typed_terminal_outcome`;
- `conformance_pool_lease_drop_releases_capacity_once`;
- `conformance_pool_explicit_release_decrements_capacity_once`;
- `conformance_pool_idle_replacement_preserves_capacity_accounting`;
- `conformance_pool_admission_refusal_is_explicit`;
- `conformance_stale_generation_output_is_protocol_failure`.

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
RestartExhausted
```

`g in G`, `r in Req`, `b` is the finite sequence of rows and progress/diagnostic events already delivered for the
current request, and `c` is an observed child exit or fatal condition. The current implementation keeps process handles,
stdio pipes, and stats as separate fields, but its private `WorkerSupervisorState` records the model-facing phases:
idle, busy, streaming, stopping, killing, reaping, crashed, restart-exhausted, and exited. Spawn-time `Starting` remains
inside `LeanWorker::spawn` rather than as a stored state on a constructed supervisor.

The core transitions are:

- `Absent -> Starting -> Idle(g)` when the supervisor spawns a child and completes the protocol handshake.
- `Idle(g) -> Busy(g, r)` when the parent admits and writes request `r`.
- `Busy(g, r) -> Streaming(g, r, b)` when the child emits a non-terminal row, diagnostic, or progress event.
- `Streaming(g, r, b) -> Streaming(g, r, b ++ [e])` for additional non-terminal events.
- `Busy(g, r)` or `Streaming(g, r, b) -> Idle(g)` when a terminal success response is accepted.
- `Busy(g, r)` or `Streaming(g, r, b) -> Killing(g) -> Reaping(g)` on timeout, in-flight RSS hard limit, or
  cancellation observed after dispatch.
- `Idle(g) -> Stopping(g) -> Reaping(g)` on explicit shutdown, graceful policy cycle, request-count cycle,
  import-count cycle, idle cycle, or RSS-ceiling cycle.
- Any state with a live child may move to `Crashed(g, c) -> Reaping(g)` when the child exits or the protocol stream
  ends unexpectedly.
- `Reaping(g) -> Starting -> Idle(g + 1)` only when restart admission succeeds.
- `Reaping(g) -> RestartExhausted` when restart intensity refuses a replacement.

## Pool Transition System

A pool state is a tuple:

```text
(capacity, entries, leases, waiting, shutting_down, restart_budget, memory_facts)
```

`capacity` is `LeanWorkerPoolConfig::max_workers`. `entries` is the finite set of local worker entries keyed by
`LeanWorkerSessionKey`. `leases` maps live lease tokens to entries. `waiting` is either empty or a caller currently
blocked in synchronous admission; it is not a durable queue. `shutting_down` records whether the pool is being dropped
or no longer admits work. `restart_budget` is the per-worker restart-intensity window. `memory_facts` contains known
RSS samples and configured total/per-worker RSS ceilings, with an explicit unavailable state on platforms where RSS
cannot be sampled.

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
fatal exit, and `Drop` all drive the child toward a terminal reaped state as far as the parent process is alive and
Tokio/Rust cleanup can run.

**Restart intensity.** Replacement attempts are bounded by a moving window. The default policy admits at most 16
restarts per 60 seconds; exhaustion returns `RestartLimitExceeded`, leaves the supervisor not accepting more work, and
requires service-level policy to create a fresh worker or pool entry.

**Streaming commit.** Rows are visible to the sink immediately. Rows delivered before terminal success are tentative.
Cancellation, timeout, fatal exit, stale request or generation, row-decode failure, sink panic, or shutdown failure
prevents the worker from claiming that those rows are committed. Downstream callers own commit, deduplication, and cache
validity.

**No stale rows after reset.** Rows from an old generation are not allowed to satisfy a request in a replacement
generation. A replacement drops old pipes, joins the old reader path, and tags parent-side reader events by generation.

## Lifecycle Semantics

**Request lifecycle.** A request begins only after policy checks and successful parent-side admission. The child
processes framed requests serially from stdin. A terminal response, EOF/fatal child exit, timeout, cancellation,
RSS hard-limit kill, protocol error, sink panic, or shutdown path ends the request at the parent boundary.

**Shutdown semantics.** `LeanWorker::shutdown` is the structured public shutdown path. It sends `Request::Terminate`,
waits up to `LEAN_WORKER_SHUTDOWN_TIMEOUT_DEFAULT` by default, escalates to `kill`, then waits up to
`LEAN_WORKER_KILL_WAIT_TIMEOUT_DEFAULT` for the process to be reaped. The returned `LeanWorkerShutdownReport` records
whether the child had already exited, stopped gracefully, or required kill escalation. `terminate` remains a deprecated
compatibility wrapper over this structured path.

**Drop semantics.** `Drop for LeanWorker` invokes the same bounded shutdown machinery. Rust `Drop` cannot return
protocol, kill, or wait failures, so callers needing status must call `shutdown`. Drop is a cleanup obligation, not a
status-reporting API.

**Parent-death handling.** Portable child exit relies on protocol pipes: stdin EOF or failed stdout writes make the
child protocol loop return and the process exit. On Linux, the child also installs `PR_SET_PDEATHSIG(SIGTERM)` and
checks the `fork/exec` race where it has already been reparented. There is no cross-platform parent-death signal or
process-group contract; hosts that need stronger behavior must use an OS process manager.

**Restart semantics.** Crash, timeout, cancellation after dispatch, shutdown, explicit cycle, memory cycling, and
restart-limit exhaustion all end the current generation. Only a successfully admitted replacement can create
generation `g + 1`, and future work must be explicitly admitted in that generation.

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

## Operational Assumptions

- OS process signaling, pipe EOF, broken-pipe behavior, and RSS sampling follow the platform contracts available to
  Rust and libc on the target system.
- Tokio and standard Rust synchronization eventually schedule parent cleanup tasks while the parent process remains
  alive.
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

## Migration Notes For Later Prompts

- `supervisor.rs` must implement `Worker transition system`, `Terminal outcome uniqueness`, `Generation separation`,
  `Parent-owned shutdown`, `Restart intensity`, and `Parent-death handling` as parent-observed lifecycle clauses.
- `session.rs` must preserve the process-safe session subset, cancellation boundary, streaming row tentativeness, and
  affine sink/lease behavior exposed through worker sessions.
- `pool.rs` must implement `Pool transition system`, `Affine lease law`, bounded admission, memory-policy refusal, and
  lease invalidation after lifecycle transitions.
- `lean-rs-worker-child` must implement the serial child request loop, terminal protocol responses, portable
  pipe-loss exit, and platform-specific parent-death hardening where available.
- `lean-rs-worker-protocol` must keep request, response, diagnostic, progress, row, stream-summary, and fatal-exit
  types compatible with the serial request model unless a future prompt first extends this document.

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

## Proof Status

This document is the mathematical runtime model used by the Rust implementation. The approved proof home is
[`31-runtime-model-proof-home.md`](31-runtime-model-proof-home.md), and the first checked Lean skeleton lives under
`formal/RuntimeModel/`. Mechanized proofs should preserve these labels or update this document first, then prove a
`Step` relation and trace-refinement theorem for the worker and pool transition systems.
