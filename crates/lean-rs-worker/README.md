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

Run the worked example:

```sh
cargo run -p lean-rs-worker --example worker_streaming
```

The recipe is
[`docs/recipes/worker-process-boundary.md`](../../docs/recipes/worker-process-boundary.md).
