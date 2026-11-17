# Diagnostics and Observability

`lean-rs` and `lean-rs-host` classify recoverable errors with [`LeanDiagnosticCode`](https://github.com/jcreinhold/lean-rs/blob/main/crates/lean-rs/src/error/mod.rs)
and emit `tracing` events against the `lean_rs` target. Branch on `err.code()` for a stable failure family; internal
stage tags can grow. Worker process outcomes use the separate `LeanWorkerError` surface. Lean runtime panics can
terminate a process and are not recoverable session errors; see
[panic containment](architecture/06-panic-containment.md).

## Diagnostic codes

The enum is non-exhaustive. Existing `as_str()` identifiers are stable across patch releases.

| Code | `as_str()` | Meaning and response |
| --- | --- | --- |
| `RuntimeInit` | `lean_rs.runtime_init` | Runtime/thread initialization failed. Check toolchain support and initialization diagnostics. |
| `Linking` | `lean_rs.linking` | Missing or incompatible artifact, identifier, initializer, or header digest. Rebuild against the host toolchain. |
| `ModuleInit` | `lean_rs.module_init` | Dylib open/initialization or project-root failure. Inspect capability preflight and the Lake root. |
| `SymbolLookup` | `lean_rs.symbol_lookup` | Missing export or incompatible lookup shape. Check the Lean library, `@[export]`, and declared signature. |
| `AbiConversion` | `lean_rs.abi_conversion` | Wrong Lean kind, range/UTF-8 failure, or missing queried declaration. Check the encoder and requested target type. |
| `LeanException` | `lean_rs.lean_exception` | Lean raised through `IO`. Inspect `LeanException::kind()` and its bounded message. |
| `Elaboration` | `lean_rs.elaboration` | Parsing/elaboration diagnostics. Inspect severity, position, file label, message, and truncation. |
| `Unsupported` | `lean_rs.unsupported` | The shim/service is unavailable. Check the loaded contract and optional operation support. |
| `Cancelled` | `lean_rs.cancelled` | A host cancellation check observed a cancelled token. Use a fresh token for retry. |
| `ResourceExhausted` | `lean_rs.resource_exhausted` | A configured budget refused or bounded work. Inspect resource facts before retrying. |
| `Internal` | `lean_rs.internal` | A Rust invariant failed or a callback panic was caught. Report the code and bounded message. |

`LeanElabFailure::code()` is always `Elaboration`. `LeanMetaResponse::code()` returns `None` for success,
`Some(Unsupported)` for a missing service, and `Some(Elaboration)` for its other failure forms.

```rust
use lean_rs::{LeanDiagnosticCode, LeanError};

fn report(err: &LeanError) {
    match err.code() {
        LeanDiagnosticCode::Linking => eprintln!("rebuild the capability: {err}"),
        LeanDiagnosticCode::ModuleInit => eprintln!("check capability preflight: {err}"),
        LeanDiagnosticCode::LeanException => {
            if let LeanError::LeanException(exc) = err {
                eprintln!("Lean raised {:?}: {}", exc.kind(), exc.message());
            }
        }
        other => eprintln!("{other}: {err}"),
    }
}
```

Same-process `HostFailure` can carry `ResourceExhaustedFacts`. Worker errors expose `LeanWorkerResourceExhaustedFacts`
through `resource_exhausted_facts()`; degraded module-query batches can carry them in cache facts.
`work_entered_lean=false` or `work_entered_child=false` means admission refusal. A true value means work entered
Lean/the child and was interrupted or degraded. Preserve that distinction; resource failures are not empty query
results. See [production hosting](production-hosting.md).

## Loader preflight

Check a shipped capability before opening its dylibs:

```rust
let report = lean_rs::LeanCapabilityPreflight::new(
    lean_rs::LeanBuiltCapability::manifest_path(env!("LEAN_RS_CAPABILITY_MY_CAPABILITY_MANIFEST")),
)
.check();
if let Some(check) = report.first_error() {
    eprintln!("{}: {}", check.code(), check.repair_hint());
}
```

`LeanCapability::from_build_manifest` also uses preflight when it can explain a load failure. Its broad error remains
`ModuleInit`; the message includes the loader code and repair hint. The source catalogue is
[`loader.rs`](https://github.com/jcreinhold/lean-rs/blob/main/crates/lean-toolchain/src/loader.rs). All identifiers below start with `lean_rs.loader.`.

| Code | Identifier suffix | Repair |
| --- | --- | --- |
| `MissingManifest` | `missing_manifest` | Package the manifest emitted by `CargoLeanCapability`. |
| `MalformedManifest` | `malformed_manifest` | Regenerate valid manifest JSON with matching helpers. |
| `UnsupportedManifestSchema` | `unsupported_manifest_schema` | Use matching helper/runtime versions. |
| `MissingPrimaryDylib` | `missing_primary_dylib` | Package the primary shared library. |
| `MissingTransitiveDependency` | `missing_transitive_dependency` | Package every recorded dependency dylib. |
| `UnsupportedArchitecture` | `unsupported_architecture` | Rebuild for the current target. |
| `UnsupportedToolchainFingerprint` | `unsupported_toolchain_fingerprint` | Rebuild against a supported Lean ABI. |
| `StaleManifest` | `stale_manifest` | Rebuild manifest and dylib together. |
| `MissingInitializer` | `missing_initializer` | Check package/module names and the shared target. |
| `MissingImportedSymbol` | `missing_imported_symbol` | Regenerate the complete dependency set. |

In the shipped-capability flow, loader search failures are artifact-manifest or packaging failures. Repair that bundle
rather than requiring users to set `LD_LIBRARY_PATH` or `DYLD_*`.

## Worker bootstrap and checked commands

`LeanWorkerCapabilityBuilder::check()` validates the child executable, capability preflight, handshake, import session,
and optional metadata expectation:

```rust
let report = builder.check();
if let Some(check) = report.first_error() {
    eprintln!("{}: {}", check.code(), check.repair_hint());
}
```

The returned `LeanWorkerBootstrapReport` uses these identifiers with prefix `lean_rs.worker.bootstrap.`:

| Code | Identifier suffix | Repair |
| --- | --- | --- |
| `WorkerChildUnresolved` | `child_unresolved` | Ship or locate the app-owned worker binary. |
| `WorkerChildNotExecutable` | `child_not_executable` | Build/package the child with executable permissions. |
| `WorkerHandshakeFailed` | `handshake_failed` | Use a compatible child calling `run_worker_child_stdio()`. |
| `CapabilityMetadataMismatch` | `metadata_mismatch` | Select the intended capability package/metadata. |
| `WorkerStartupFailed` | `startup_failed` | Inspect the bounded startup message in the deployment environment. |

`CapabilityPreflight { code }` carries the corresponding `lean_rs.loader.*` identifier instead. Worker command lookup
uses `lean_rs.worker.checked_binding` when manifest identity, export metadata, or the trusted signature is missing or
mismatched. The message names the export and shape, such as metadata/JSON `String -> IO String` or streaming
`String, USize, USize -> IO UInt8`. Rebuild with `CargoLeanCapability::export_signature(...)` or the builder's command
helpers; the check fails before invoking the export.

`lean-toolchain::LinkDiagnostics` is a separate build-script enum for discovery, unsupported toolchains, missing Lake or
targets, build failure, and unresolved outputs. It renders as one line suitable for an error or `cargo:warning=`.

## Coordinates and proof candidates

Worker diagnostics mark their coordinate space. `original_source` refers to the caller's editable source; an available
`original_range` includes that range and file label. `synthetic_buffer` refers to generated source, such as a candidate
overlay: flat line/column fields are display positions and `original_range` is absent. Missing metadata from older
frames decodes as `unknown`.

`LeanWorkerSession::attempt_proof` evaluates candidates in request order against one source/edit target. The shim builds
the base snapshot and resolves the position once, then elaborates isolated in-memory overlays. It writes no candidate
files; a candidate-local failure does not poison later candidates.

| Row status | Meaning |
| --- | --- |
| `closed` | Closed the selected goal. |
| `progressed` | Elaborated with goals remaining. |
| `failed` | Candidate-local diagnostics include an error. |
| `timeout` | Heartbeats were exhausted. |
| `budget_exceeded` | Attempted, but its projected row exceeded the output budget. |
| `not_attempted` | Within the count cap, but skipped after an earlier row exhausted the output budget. |
| `unsupported` | The shim lacks proof-attempt support. |

`candidate_limit` reports the shim's count cap. Inputs beyond it are omitted with `candidates_truncated=true`; resubmit
a smaller batch to obtain those outcomes. Within the cap, completed rows stay ordered and budget-skipped candidates
remain explicit. `ModuleQueryOutputBudgets` bounds rendered fields and total projected payload; heartbeats apply per
candidate. A parent timeout/cancellation returns `LeanWorkerError` and discards the child generation according to
lifecycle policy. Earlier rows then do not constitute a committed `ProofAttemptResult`.

## Streams and pool snapshots

Process exit, request timeout, cancellation-triggered cycling, stale lease use, sink panic, and typed-row decode failure
are worker outcomes. Use `LeanWorkerPoolSnapshot` and lease `snapshot()` to inspect active workers, warm leases, restart
reasons, best-effort RSS, stream outcomes, rows, bytes, elapsed time, and backpressure counters. The queue-depth field
is currently zero: admission is synchronous and bounded by `queue_wait_timeout`, without a request mailbox.

Rows reach sinks before terminal success. An empty successful stream has a zero-row terminal summary. If a later request
error occurs, delivered rows remain partial and tentative. Inspect `stream_failures`, `data_rows_delivered`,
`data_row_payload_bytes`, and `backpressure_waits`/`backpressure_failures` alongside the returned error. Commit counts
from a successful terminal summary, rather than from callbacks alone.

## Callbacks and cancellation

`Internal` covers caught Rust callback panics, not Lean panic recovery. Same-process shims return callback-local
`LeanCallbackStatus` values:

| Status | Value | Meaning |
| --- | ---: | --- |
| `Ok` | 0 | Continue. |
| `StaleHandle` | 1 | The handle was dropped. |
| `Panic` | 2 | Rust caught the callback panic and recorded an error on the live handle. |
| `WrongPayload` | 3 | The trampoline payload did not match the handle. |
| `Stopped` | 4 | The callback requested a clean stop. |

Progress sink panics become `LeanError::Host`, stage `CallbackPanic`, code `lean_rs.internal`. Cancellation is checked
when Rust regains control; it cannot pre-empt an in-flight Lean call. See
[callbacks](architecture/10-callback-registry.md), [progress](architecture/13-structured-progress.md), and
[cancellation](architecture/07-cooperative-cancellation.md).

## Tracing

The crates do not install a subscriber. Configure one downstream, for example:

```rust
tracing_subscriber::fmt()
    .with_env_filter(
        tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("lean_rs=info")),
    )
    .init();
```

Use `lean_rs=info,lean_toolchain=info` for initialization/imports, `debug` for library and host dispatch, or `trace` for
typed-export and decoder detail. Common names follow; the source instrumentation is authoritative for additional spans
and fields. `source_len` is bytes.

| Span/event | Level | Fields |
| --- | --- | --- |
| `lean_rs.runtime.init` | info | — |
| `lean_rs.module.library.open` / `.open_globally` | debug | Shortened `path` |
| `lean_rs.module.library.initialize` | debug | `package`, `module` |
| `lean_rs.module.initializer.call` | debug | `initializer` |
| `lean_rs.module.exported.call` | trace | `arity` |
| `lean_rs.abi.decode` | trace | `shape`, `len` |
| `lean_rs.host.session.import` | info | `imports_len` |
| `lean_rs.host.session.query_declaration` | debug | `name` |
| `lean_rs.host.session.list_declarations_filtered` | debug | Inclusion flags |
| `lean_rs.host.session.declaration_type` / `_kind` / `_name` | debug | `name` |
| Corresponding `_bulk` session spans | debug | `batch_size` |
| `lean_rs.host.session.elaborate` / `.kernel_check` | debug | `source_len`, `heartbeats`, `diagnostic_byte_limit` |
| `lean_rs.host.session.run_meta` | debug | `service`, `heartbeats`, `diagnostic_byte_limit` |
| `lean_rs.host.session.process_module_query_batch` | debug | Source/selector counts and output/Lean budgets |
| `lean_rs.host.session.attempt_proof` | debug | Source/candidate counts and output/Lean budgets |
| `lean_rs.host.pool.acquire` | debug | `imports_len` |
| `lean_rs.host.pool.acquire.result` | debug | `hit` |
| `lean_rs.host.pool.release` | trace | `kept` |

### Capturing diagnostics in tests

`DiagnosticCapture` buffers `lean_rs` spans/events on the installing thread and restores the prior subscriber on drop:

```rust
use lean_rs::{DiagnosticCapture, LeanDiagnosticCode};

#[test]
fn captures_diagnostic_code() {
    let capture = DiagnosticCapture::install();
    tracing::warn!(target: "lean_rs", code = "lean_rs.module_init", "fixture failure");
    assert!(capture.events().iter().any(|event|
        event.code == Some(LeanDiagnosticCode::ModuleInit)
    ));
}
```

The guard is `!Send + !Sync`. Default capacity is 256 events; overflow drops the oldest and increments `overflowed()`.
Use `with_capacity(N)` to change it. While installed, the capture records only targets beginning with `lean_rs`; it does
not forward other events to the previous subscriber.

### Bounding

Lean-authored error text is bounded at construction, on a UTF-8 boundary, by the 4 KiB error-message limit. Tracing
paths normally retain the basename and two parents; full paths require explicit trace-level emission. Shortening bounds
log size and is not secret redaction. Install a downstream subscriber filter when policy requires suppressing paths.
