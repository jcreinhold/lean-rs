# lean-rs-worker

Worker-process boundary for `lean-rs` host workloads. Supervises a `lean-rs-worker-child`
process around the in-process theorem-prover stack in [`lean-rs-host`](https://docs.rs/lean-rs-host),
adding fatal-exit containment, memory cycling, request timeouts, and parent-owned row
streaming.

Use this crate when a downstream tool needs process isolation, live rows, diagnostics,
terminal summaries, request timeouts, or memory cycling. Use `lean-rs-host` directly for
trusted in-process work that does not need those guarantees.

## Quick start

```sh
cargo run -p lean-rs-worker --example worker_capability_runner
```

The runner is the normal downstream shape: `LeanWorkerCapabilityBuilder` manages startup,
typed commands carry requests and rows, diagnostics arrive on a separate sink, and explicit
cycling resets process-global memory. The full recipe is
[`docs/recipes/worker-capability-runner.md`](https://github.com/jcreinhold/lean-rs/blob/main/docs/recipes/worker-capability-runner.md).

For a shipped application, build the Lean capability in `build.rs` with
`lean_toolchain::CargoLeanCapability`, embed the resulting path with
`lean_rs::LeanBuiltCapability::path(env!(...))`, and ship an app-owned worker
child binary:

```rust,ignore
fn main() -> std::process::ExitCode {
    lean_rs_worker::run_worker_child_stdio()
}
```

Point the builder at that binary with
`LeanWorkerChild::sibling("my_app_lean_worker").env_override("MY_APP_LEAN_WORKER")`.
See
[`docs/recipes/ship-crate-with-lean.md`](https://github.com/jcreinhold/lean-rs/blob/main/docs/recipes/ship-crate-with-lean.md).

## What the crate owns

| Concern | API |
| ------- | --- |
| Child process lifecycle, pipes, frame decoding, fatal-exit classification, cleanup | `LeanWorker`, `LeanWorkerConfig`, `LeanWorkerError`, `LeanWorkerExit`, `LeanWorkerStats` |
| Restart and memory-cycling policy: explicit cycling, request-count threshold, import-like threshold, idle interval, RSS ceiling | `LeanWorkerRestartPolicy`, `LeanWorkerRestartReason` |
| Capability startup: Lake build, child resolution, health-check, import session, optional metadata validation | `LeanWorkerCapabilityBuilder` |
| Packaged worker child resolution | `LeanWorkerChild`, `run_worker_child_stdio` |
| Typed downstream commands and row streams | `LeanWorkerJsonCommand<Req, Resp>`, `LeanWorkerStreamingCommand<Req, Row, Summary>`, `LeanWorkerTypedDataSink`, `LeanWorkerDiagnosticSink` |
| Narrow host-session adapter (elaboration, kernel-check status, declaration-kind/name bulk queries) | `LeanWorker::open_session` |
| Local multi-worker fanout, warm reuse, fixed admission, RSS-aware policy, lease invalidation | `LeanWorkerPool`, `LeanWorkerSessionLease`, `LeanWorkerSessionKey`, `LeanWorkerPoolSnapshot` |
| Import planning: Lake module discovery to stable pool-execution batches | `LeanWorkerImportPlanner`, `LeanWorkerPlannedBatch` |
| Progress and cancellation across the IPC boundary | `LeanWorkerProgressEvent`, `LeanWorkerError::Cancelled` |

## What the crate does not own

- Downstream command names, request schemas, row schemas, summary schemas.
- Cache validity, ranking, reporting, source provenance, CLI policy.
- Child PIDs, worker IDs, pipes, protocol frames, or which warm worker the pool selected.
- Remote (cross-host) workers, byte callbacks, object callbacks. These are non-goals for the
  current local-scale release; see
  [`docs/architecture/28-production-scale-release.md`](https://github.com/jcreinhold/lean-rs/blob/main/docs/architecture/28-production-scale-release.md).

## Key behaviors

**Cycling is the reset boundary for Lean process-global memory.** A `LeanWorkerRestartPolicy`
cycles the child before requests when a configured request count, import-like request count,
RSS ceiling, or idle duration is reached. `SessionPool::drain()` is an in-process cache
operation, not an RSS reset.

**Startup and request timeouts are independent.** Startup timeout covers the child
handshake. Request timeout covers one request after its frame is written, including live
rows, diagnostics, progress, and terminal response. On request-timeout expiry the supervisor
kills and replaces the child, records `LeanWorkerRestartReason::RequestTimeout`, returns
`LeanWorkerError::Timeout`, and invalidates the open worker session.

**Row delivery uses bounded internal buffering.** Slow sinks block the request path instead
of growing memory without bound. Rows are never dropped for committed streams. Delivered
rows are tentative until terminal success returns `LeanWorkerTypedStreamSummary` with total
rows, per-stream counts, elapsed time, and optional typed metadata.

**Large row streams use a raw-JSON payload fast path.** The child validates payload as JSON,
the protocol carries it without building a `serde_json::Value` tree, and typed commands
deserialize directly into the caller row type at the parent boundary. `LeanWorkerDataRow`
remains the schema-less escape hatch when callers want arbitrary inspection rather than
maximum throughput.

**RSS sampling is best-effort and platform-specific.** Unavailable samples are recorded as
unavailable, not as a false claim that the pool is under budget.

**Capability metadata and doctor checks are separate from row streams.**
`LeanWorker::runtime_metadata` reports protocol facts from the child handshake.
`LeanWorkerSession::capability_metadata` and `LeanWorkerSession::capability_doctor` call
downstream exports with ABI `String -> IO String`, decode the returned JSON into generic
command, capability, version, and diagnostic envelopes, and leave cache policy and semantic
command meaning to the downstream tool.

## Lean-side helpers

Lean capability packages should use `LeanRsInterop.Worker.Stream` from
`lean-rs-interop-shims` when emitting worker streams. The helpers build row, diagnostic,
progress, terminal-metadata, and status envelopes for the child-local string callback path.
They do not define downstream row schemas, command names, session keys, or pool scheduling
policy.

## More examples

| Command | Purpose |
| ------- | ------- |
| `cargo run -p lean-rs-worker --example worker_streaming` | Typed streaming command with parent-side watchdog and worker cycling. |
| `cargo run --release -p lean-rs-worker --example worker_capability_probe` | Operational probe over generic command shapes (`version`, `doctor`, `extract`, `features`, `index`, `probe`); records cold startup, first import, cancellation latency, fatal-exit recovery, cycling, throughput, RSS. Set `LEAN_RS_WORKER_COMPARE_COMMAND=…` to time a comparison command alongside. |
| `cargo run -p lean-rs-worker --example worker_pool` | Pool, lease acquisition, typed streaming command, leased-worker cycling. |
| `cargo run -p lean-rs-worker --example pool_memory_scheduling` | Pool memory-aware admission and cycling under a small import policy. Records parent RSS, child RSS snapshots, unavailable samples, budget admission, policy restarts. |
| `cargo run -p lean-rs-worker --example mathlib_scale_probe` | Planner → pool → lease → typed command path. Set `LEAN_RS_MATHLIB_ROOT=/path/to/mathlib4` to use a real module list as planning workload. |
| `cargo run -p lean-rs-worker --example lean_dup_readiness` | Readiness fixture over all command shapes. Set `LEAN_RS_LEAN_DUP_ROOT=/path/to/checkout` to record the comparison checkout's revision; set `LEAN_RS_WORKER_COMPARE_COMMAND=…` to time a comparison command. |

The `mathlib_scale_probe` and `pool_memory_scheduling` examples need the worker child built
first: `cargo build -p lean-rs-worker --bin lean-rs-worker-child`.

## Recipes and contracts

- [`docs/recipes/worker-capability-runner.md`](https://github.com/jcreinhold/lean-rs/blob/main/docs/recipes/worker-capability-runner.md): normal downstream path.
- [`docs/recipes/worker-process-boundary.md`](https://github.com/jcreinhold/lean-rs/blob/main/docs/recipes/worker-process-boundary.md): lower-level process-boundary recipe.
- [`docs/architecture/28-production-scale-release.md`](https://github.com/jcreinhold/lean-rs/blob/main/docs/architecture/28-production-scale-release.md): local scale contract and non-goals.

## License

Dual-licensed under MIT or Apache-2.0 at your option.
