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
boundary. `LeanWorkerSession::run_data_stream` runs a fixed-ABI Lean export in
the child, validates each callback string as either a data row, diagnostic, or
terminal metadata envelope, assigns per-stream sequence numbers, and reports
owned `LeanWorkerDataRow` values to a borrowed `LeanWorkerDataSink`.
Diagnostics use `LeanWorkerDiagnosticSink`, not row payloads. Delivered rows are
tentative until terminal success returns `LeanWorkerStreamSummary` with total
rows, per-stream counts, elapsed time, and optional metadata. Row schemas belong
to the downstream tool.

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

The recipe is
[`docs/recipes/worker-process-boundary.md`](../../docs/recipes/worker-process-boundary.md).
