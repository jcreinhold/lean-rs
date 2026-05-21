# lean-rs-worker

Worker-process boundary for `lean-rs` host workloads.

`LeanWorker` supervises one `lean-rs-worker-child` process. It hides process
spawning, stdio pipes, protocol framing, child-exit parsing, and cleanup behind
a small lifecycle API. The in-process theorem-prover stack remains in
`lean-rs-host`; this crate owns the production process boundary around it.

`LeanWorkerRestartPolicy` cycles the child process before requests when a
configured request count, import-like request count, RSS ceiling, or idle
duration is reached. Cycling is a process restart. It is the reset boundary for
Lean process-global runtime and import memory; `SessionPool::drain()` remains an
in-process cache operation, not an RSS reset.

`LeanWorkerCapabilityBuilder` is the normal downstream entry point. It builds
the named Lake `lean_lib` shared target through `lean-toolchain`, resolves the
worker child, starts and health-checks the worker, opens the configured import
session, and can validate generic capability metadata. Callers provide the Lake
project root, package name, library target, imports, and deliberate policy
overrides; they do not construct `.lake/build/lib` paths or hand-order startup
steps. The default child resolver checks `LEAN_RS_WORKER_CHILD`, sibling Cargo
profile paths, and the in-tree workspace development build. Packaged
applications may set `LEAN_RS_WORKER_CHILD` or pass `worker_executable` when
the child binary is shipped elsewhere.

`LeanWorkerPool` is the local multi-worker entry point for scale work. Callers
acquire a `LeanWorkerSessionLease` from capability requirements and then run
typed commands through the lease. The pool reuses compatible warm workers,
replaces dead workers, enforces a fixed local worker limit, applies optional
memory-aware admission and cycling policy, and invalidates leases after timeout,
cancellation, child failure, explicit cycle, metadata mismatch, or policy cycle.
The pool can bound known total child RSS, cycle warm workers that exceed a
per-worker RSS ceiling, cycle idle workers, and bound synchronous admission waits
for a full pool. RSS sampling is best effort; unavailable samples are recorded
rather than presented as false memory claims. The pool does not expose child
pids, worker ids, pipes, protocol frames, or which warm worker was selected. A
`LeanWorkerSessionKey` is only a worker reuse key; downstream tools still own
row schemas, cache validity, ranking, reporting, and source provenance.

`LeanWorkerImportPlanner` groups Lake modules into stable pool-execution
batches. It consumes `lean-toolchain` module discovery, checks that the
capability target exists, and produces `LeanWorkerPlannedBatch` values keyed by
the same session material as `LeanWorkerPool`: project root, package, target,
source root, imports, metadata expectation, toolchain fingerprint, and restart
policy class. Planned batches contain no worker ids or scheduling decisions.
They help callers reuse imports; they are not downstream cache entries.

Startup timeout and request timeout are separate. Startup timeout covers the
child handshake. Request timeout covers one request after its frame is written,
including live rows, diagnostics, progress, and terminal response. If the
request deadline expires, the supervisor kills and replaces the child, records
`LeanWorkerRestartReason::RequestTimeout`, returns `LeanWorkerError::Timeout`,
and invalidates the open worker session.

`LeanWorker::open_session` adds a narrow host-session adapter over the worker
boundary. It supports elaboration, kernel-check status, declaration-kind bulk
queries, and declaration-name bulk queries. The adapter returns copied
diagnostics and strings; it does not send child-runtime handles such as
`LeanExpr`, `LeanEvidence`, or `LeanDeclaration` to the parent.

Worker progress and cancellation are parent-side IPC concepts. Progress arrives
as `LeanWorkerProgressEvent`; in-flight cancellation cycles the child process,
returns `LeanWorkerError::Cancelled`, and invalidates the open worker session.

Worker data rows carry downstream-owned JSON payloads over the same process
boundary. The normal downstream path is the typed command facade:
`LeanWorkerJsonCommand` for one JSON response and `LeanWorkerStreamingCommand`
for live row streams. Callers provide serde request, row, and summary types;
`lean-rs-worker` owns transport, diagnostics, timeout, cancellation, completion,
and decode-error context. `LeanWorkerSession::run_data_stream` remains the raw
row escape hatch for fixtures and unusual callers.

A streaming command runs a fixed-ABI Lean export in the child, validates each
child-local callback string as either a data row, diagnostic, or terminal
metadata envelope, assigns per-stream sequence numbers, decodes row payloads
into the caller's row type, and reports typed rows to a borrowed
`LeanWorkerTypedDataSink`.
Diagnostics use `LeanWorkerDiagnosticSink`, not row payloads. Delivered rows are
tentative until terminal success returns `LeanWorkerTypedStreamSummary` with
total rows, per-stream counts, elapsed time, and optional typed metadata. Row
schemas belong to the downstream tool.

Large row streams use a private raw-JSON payload path. The child validates the
payload as JSON, the worker protocol carries it without building a
`serde_json::Value` tree, and typed commands deserialize directly into the
caller row type at the parent boundary. `LeanWorkerDataRow` remains the
schema-less escape hatch with a `serde_json::Value` payload; use it when callers
need arbitrary inspection rather than maximum throughput.

Lean capability packages should use the generic
`LeanRsInterop.Worker.Stream` helpers from `lean-rs-interop-shims` when
emitting worker streams. Those helpers build row, diagnostic, progress,
terminal metadata, and status envelopes for the child-local string callback
path. They do not define downstream row schemas, command names, session keys,
or pool scheduling policy.

Capability metadata and doctor checks are separate from row streams.
`LeanWorker::runtime_metadata` reports `lean-rs-worker` protocol facts from the
child handshake. `LeanWorkerSession::capability_metadata` and
`LeanWorkerSession::capability_doctor` call downstream exports with ABI
`String -> IO String`, decode the returned JSON into generic command,
capability, version, and diagnostic envelopes, and leave cache policy and
semantic command meaning to the downstream tool.

Run the worked example:

```sh
cargo run -p lean-rs-worker --example worker_streaming
```

Run the worker capability recipe example:

```sh
cargo run -p lean-rs-worker --example worker_capability_runner
```

This is the normal downstream shape: builder-managed startup, typed commands,
live rows, diagnostics, progress, terminal completion, request timeout
handling, and explicit worker cycling.

Run the downstream-shaped operational probe:

```sh
cargo run --release -p lean-rs-worker --example worker_capability_probe
```

That probe uses command-like fixture exports named `version`, `doctor`,
`extract`, `features`, `index`, and `probe` to exercise the generic worker
capability layer. The row schemas stay deliberately small and generic; they are
not `lean-dup` declarations or feature rows. The probe records cold startup,
first import, import-once streaming, cancellation latency, fatal-exit recovery,
worker cycling, row throughput, and parent/child RSS samples. To compare
against an existing downstream subprocess worker in a local checkout, set
`LEAN_RS_WORKER_COMPARE_COMMAND` to the shell command you want timed; the probe
reports the command status and elapsed time without treating that command as
part of the `lean-rs-worker` API.

Run the local pool lease example:

```sh
cargo run -p lean-rs-worker --example worker_pool
```

That example opens a pool, acquires a compatible lease, runs a typed streaming
command, cycles the leased worker, and leaves worker selection hidden behind
the lease.

Run the pool memory-scheduling workload:

```sh
cargo build -p lean-rs-worker --bin lean-rs-worker-child
cargo run -p lean-rs-worker --example pool_memory_scheduling
```

That workload records parent RSS, child RSS snapshots, unavailable RSS samples,
budget admission behavior, policy restarts, fixture import reuse, a documented
mathlib-shaped fallback, and repeated cycle/reuse under a small import policy.
The numbers are workload evidence, not a claim that `SessionPool::drain()` can
reset Lean process-global RSS.

Run the mathlib-scale fixture probe:

```sh
cargo build -p lean-rs-worker --bin lean-rs-worker-child
cargo run -p lean-rs-worker --example mathlib_scale_probe
```

That probe exercises the planner -> pool -> session lease -> typed command path
with mixed declaration-like, feature-like, and probe-like rows, diagnostics,
progress, terminal metadata, cancellation, fatal-exit recovery, and worker
cycling. Set `LEAN_RS_MATHLIB_ROOT=/path/to/mathlib4` to use a discovered
mathlib module list as the workload shape. The fixture rows remain generic test
data, not downstream schemas.

The recipe is
[`docs/recipes/worker-capability-runner.md`](../../docs/recipes/worker-capability-runner.md).
The lower-level process-boundary recipe is
[`docs/recipes/worker-process-boundary.md`](../../docs/recipes/worker-process-boundary.md).
