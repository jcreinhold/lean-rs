# Same-Process Controls

`lean-rs-host` bounds cooperative work, output, caches, and a closed metadata query. Worker children provide the process
reset boundary for import-heavy or potentially wedged work; see [production hosting](production-hosting.md).

| Control | Public surface | Boundary |
| --- | --- | --- |
| Heartbeats | `LeanElabOptions`, `LeanMetaOptions` | Operations that check `Lean.maxHeartbeats`; no hard preemption. |
| Cancellation | `LeanCancellationToken` | Rust dispatch boundaries and cancellable bulk items; no interruption of an in-flight Lean call. |
| Diagnostic bytes | Elaboration/meta `diagnostic_byte_limit` | Returned diagnostic collection, rather than all Lean allocations. |
| Render/output bytes | `ModuleQueryOutputBudgets`, `DeclarationInspectionBudgets` | Projected module-query, proof-attempt, verification, and inspection output. |
| Search rows | `DeclarationSearchRequest::limit` | Result count; Lean also clamps broad requests internally. |
| Snapshot cache clearing | `LeanSession::clear_module_snapshot_cache` | Shim-owned snapshots, rather than runtime reset or module unloading. |
| Import facts | `LeanSession::import_stats`, `LeanImportStats::memory_diagnostic` | Attribution for compacted regions, extensions, and bytes. |
| Import profile | `LeanSessionImportProfile` | Which declaration data a full-session import loads. |
| Bracketed import | `LeanCapabilities::bracketed_import_query` | One closed declaration-metadata query without loaded extensions. |

## Cancellation

`LeanCancellationToken` is a one-way, clonable `Arc<AtomicBool>` flag and is `Send + Sync`. Pass `Some(&token)` to a
host operation; another thread or a progress sink may call `cancel()` on a clone. Observed cancellation returns
`LeanError::Cancelled`, code `lean_rs.cancelled`. Create a fresh token for the next request. Sessions, capabilities, and
Lean handles remain thread-bound.

```rust
let token = lean_rs_host::LeanCancellationToken::new();
let canceller = token.clone();
std::thread::spawn(move || canceller.cancel());

// On the thread that owns the session:
let declarations = session.query_declarations_bulk(&names, Some(&token), None)?;
```

Host query, elaboration, kernel, meta, and proof-workflow calls check the token before dispatch, with further checks
where Rust controls multiple dispatch points. With a token, `query_declarations_bulk`, `declaration_type_bulk`,
`declaration_kind_bulk`, `declaration_name_bulk`, and `elaborate_bulk` use singular calls in a Rust loop. They check
between items and discard collected output on cancellation. Without a token, they keep Lean-side bulk dispatch. Empty
bulk input returns without dispatch or progress events.

Fresh imports, filtered listing, kernel checking, and module processing may enter one long Lean call after the initial
check. There is no universal post-dispatch check: cancelling during that call need not turn success into `Cancelled`. A
token cannot interrupt reduction, a stuck `isDefEq`, an export's own loop, or a fatal failure. Timing out an async wait
or cancelling a join handle also leaves the underlying work running. Heartbeats bound only work that consumes them; use
[worker processes](architecture/worker-processes.md) for a wall-clock watchdog and recovery.

## Progress

Methods that report progress accept `Option<&dyn LeanProgressSink>` after the cancellation argument. `None` avoids
callback registration. `LeanProgressEvent` contains a method-local `phase: &'static str`, `current: u64`,
`total: Option<u64>`, and elapsed `Duration` for that phase. Events run synchronously on the Lean-bound calling thread.
A sink must not re-enter the session or call stack; queue expensive UI, logging, or IPC work elsewhere.

Reporting covers fresh imports, declaration lookup/type/kind/name bulk calls, bulk elaboration, filtered listing, and
kernel checking. Events describe phases, not every Lean suboperation. With progress alone, bulk calls keep one Lean-side
dispatch; declaration lookup also reports Rust's `prepare_names` phase. With a token, the per-item Rust loop reports
after each item and observes cancellation at the next check. Progress during a single Lean call adds no interruption
point. Tracing records call boundaries for logs and profiles; progress supplies live feedback.

Lean-side reporting registers a scoped `LeanProgressCallback` for one synchronous call and removes it before return.
Rust item loops report through the host panic boundary. Unwinding sink panics become `LeanError::Host` at
`HostStage::CallbackPanic`, code `lean_rs.internal`; fatal failures still need process containment.

Additive progress shims return `IO (Except UInt8 α)`. `LeanProgressCallback::decode_result` maps a `Panic` status to the
stored error. `StaleHandle`, `WrongPayload`, and `Stopped` indicate invariant failures: host progress has no generic
callback stop semantics. Existing no-progress exports retain separate signatures. Capabilities open generic interop
globally before the host library so its callback initializer is available. The
[callback and shim contract](architecture/callbacks.md) owns lifetime, payload, and package-parity rules. See the
[progress implementation](https://github.com/jcreinhold/lean-rs/blob/main/crates/lean-rs-host/src/host/progress.rs),
[session dispatch](https://github.com/jcreinhold/lean-rs/blob/main/crates/lean-rs-host/src/host/session.rs), and
[progress tests](https://github.com/jcreinhold/lean-rs/blob/main/crates/lean-rs-host/tests/progress.rs) for exact
coverage.

## Import lifetime

Full sessions use `loadExts := true` for parser, elaborator, proof-state, pretty-printer, source-range, and capability
workflows. Extension state may retain references into compacted regions. Neither session drop nor cache clearing makes
`Environment.freeRegions` safe on that path.

The bracketed query uses `withImportModules` with `loadExts := false`, serializes declaration metadata to JSON, and
frees regions before Rust parses the owned result. No environment, expression, name, declaration, extension state,
session, or capability handle may escape. It cannot support normal elaboration, parser-backed queries, or
pretty-printing, and is not a full-session import profile. A new bracketed query returning Lean-owned objects needs a
local lifetime proof before it can be safe.

Same-process controls cannot recover from native abort, process exit, foreign unwind, or a wedged Lean runtime. The
[memory evidence](safety/long-session-memory.md) records local measurements and the retained-state model.

## Verify changes

Test defaults and FFI normalization without full imports where possible. Use focused host tests for heartbeat
exhaustion, diagnostic truncation, and pre-cancelled tokens. The bracketed-import test must check `load_exts == false`,
`free_regions_ran == true`, and owned metadata. Run integration tests through `cargo nextest`; see
[testing](testing.md).
