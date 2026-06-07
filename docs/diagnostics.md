# Diagnostics and Observability

`lean-rs` and `lean-rs-host` project errors to the same stable [`lean_rs::LeanDiagnosticCode`] taxonomy and emit
structured `tracing` spans against the `lean_rs` target. Long-running host session operations can also report live
progress through `lean_rs_host::LeanProgressSink`; see
[`architecture/13-structured-progress.md`](architecture/13-structured-progress.md). The worker crates use its own
`LeanWorkerError` surface for process-boundary outcomes such as child exit, request timeout, cancellation, data-sink
panic, diagnostic-sink panic, and typed command decode failures. A downstream caller gets:

- a stable identifier (`err.code()`) to react by family, independent of internal stage tags that may grow new variants;
- visibility into where time is spent and where failures originate, without rebuilding the crates.

The taxonomy is unified across `lean-rs` and `lean-rs-host` (every recoverable failure on either side maps to a stable
code); the span catalogue is split by emitting crate so the layer boundary is visible at the log line. Lean internal
panics are outside this error taxonomy: there is no `SessionPoisoned` code. A Lean runtime panic during a `LeanSession`
call may terminate the process; see [`architecture/06-panic-containment.md`](architecture/06-panic-containment.md). When
the same failure happens inside the worker crates, the parent observes a typed worker exit instead of an in-process
`LeanDiagnosticCode`.

## Diagnostic codes

| Code | `as_str()` | Meaning | Common fix |
| --- | --- | --- | --- |
| `RuntimeInit` | `lean_rs.runtime_init` | Lean runtime init failed (panic in `lean_initialize_*`, task-manager failure, thread-attach floor). | Pin one `lean-toolchain` across the workspace. Restart the process if an earlier crash left thread-local state behind. |
| `Linking` | `lean_rs.linking` | A linkable artefact was missing or mismatched: invalid Lake package/module identifier, missing initializer symbol, header-digest mismatch. | Verify the `lib_name` matches what `lake build` emitted (inspect `.lake/build/lib/`). Rebuild the capability against the same `lean-toolchain` as the host process. |
| `ModuleInit` | `lean_rs.module_init` | A capability dylib could not be opened, parsed, or its root initializer raised. Includes Lake project root not existing. | For shipped crates, rebuild through `CargoLeanCapability` and inspect `LeanCapabilityPreflight`. For `LeanHost::from_lake_project`, verify the Lake root contains `lakefile.lean` / `lakefile.toml` and re-run `lake build`. Only low-level `LeanLibrary` callers should repair loader search paths manually. |
| `SymbolLookup` | `lean_rs.symbol_lookup` | A function or global symbol was not present in the loaded dylib (`dlsym` miss or arity mismatch). | Add the Lean module exporting the symbol to the capability's `lean_lib`. Check `@[export]` on the Lean side. Check that the Rust call site's expected arity matches the export. |
| `AbiConversion` | `lean_rs.abi_conversion` | Wrong Lean kind for the requested Rust type, integer out of range, invalid UTF-8, or a queried declaration was missing from the environment. | For `query_declaration`: compare against `session.list_declarations(None)`. For numeric overflow: widen the Rust target type or split the call. For `String`/`ByteArray`: check the Lean encoder. |
| `LeanException` | `lean_rs.lean_exception` | Lean raised through its `IO` error channel. Inspect `LeanException::kind()` for the `IO.Error` constructor. | Branch on `exc.kind()`. For `noFileOrDirectory` / `permissionDenied`, adjust the path or working directory of the Lean code. |
| `Elaboration` | `lean_rs.elaboration` | Term parsing or elaboration produced diagnostics. Payload is `LeanElabFailure` with typed diagnostics. | Walk `failure.diagnostics()`: each carries a `severity`, bounded `message`, optional `position`, and `file_label`. If `failure.truncated() == true`, raise `LeanElabOptions::diagnostic_byte_limit` and rerun. |
| `Unsupported` | `lean_rs.unsupported` | The host shim returned `unsupported` for the requested service, or an optional meta-service symbol was absent at load. | Rebuild the bundled host shims and the consumer capability with the same Lean toolchain. The fixture in this repo exercises all four meta services (`infer_type`, `whnf`, `heartbeat_burn`, `is_def_eq`). |
| `Cancelled` | `lean_rs.cancelled` | A `lean-rs-host` cooperative cancellation token was observed before a host-controlled FFI dispatch. | Treat the operation as aborted and discard partial work. Create a fresh token before retrying. |
| `ResourceExhausted` | `lean_rs.resource_exhausted` | A caller-configured resource budget was exhausted before the operation ran. | Reuse an existing session, cycle the worker process, reduce result size, or raise the explicit budget after measuring peak RSS. |
| `Internal` | `lean_rs.internal` | A `pub(crate)` invariant tripped, or a callback panicked inside the safe boundary. | File a bug; include the bounded message and the `as_str()` id. |

The enum is `#[non_exhaustive]`; new variants may be added. Variant names and `as_str()` ids are stable across patch
releases.

## Loader preflight

Manifest-backed shipped capabilities can be checked before `LeanCapability` opens any dylib:

```rust
let report = lean_rs::LeanCapabilityPreflight::new(
    lean_rs::LeanBuiltCapability::manifest_path(env!("LEAN_RS_CAPABILITY_MY_CAPABILITY_MANIFEST")),
)
.check();

if let Some(check) = report.first_error() {
    eprintln!("{}: {}", check.code(), check.repair_hint());
}
```

`LeanCapability::from_build_manifest` runs the same preflight internally when that produces a clearer failure. The broad
`LeanError` still reports `LeanDiagnosticCode::ModuleInit`, but the bounded message includes the stable loader code and
repair hint.

| Loader code | `as_str()` | Meaning | Common fix |
| --- | --- | --- | --- |
| `MissingManifest` | `lean_rs.loader.missing_manifest` | The manifest path was absent, unreadable, or pointed at a missing file. | Rebuild through `CargoLeanCapability` and package the emitted manifest. |
| `MalformedManifest` | `lean_rs.loader.malformed_manifest` | The manifest was not valid JSON or missed required fields. | Rebuild the Lean capability with the same `lean-rs` toolchain helper. |
| `UnsupportedManifestSchema` | `lean_rs.loader.unsupported_manifest_schema` | The manifest schema is not understood by this runtime crate. | Rebuild with matching `lean-toolchain` and `lean-rs` versions. |
| `MissingPrimaryDylib` | `lean_rs.loader.missing_primary_dylib` | The primary capability dylib named by the manifest is missing. | Rebuild and package the built shared library. |
| `MissingTransitiveDependency` | `lean_rs.loader.missing_transitive_dependency` | A dependency dylib named by the manifest is missing. | Package every manifest dependency, including generic interop shims when used. |
| `UnsupportedArchitecture` | `lean_rs.loader.unsupported_architecture` | A dylib is not a native object for this target architecture. | Rebuild the Lean capability for the current Cargo target. |
| `UnsupportedToolchainFingerprint` | `lean_rs.loader.unsupported_toolchain_fingerprint` | The manifest was built with a mismatched or unsupported Lean toolchain fingerprint. | Rebuild the Lean capability with the same Lean toolchain as the Rust binary. |
| `StaleManifest` | `lean_rs.loader.stale_manifest` | The manifest appears older than the primary dylib it describes. | Rebuild through `CargoLeanCapability` so the manifest and dylib match. |
| `MissingInitializer` | `lean_rs.loader.missing_initializer` | The primary dylib does not export the package/module initializer named by the manifest. | Check package/module names and rebuild the shared target. |
| `MissingImportedSymbol` | `lean_rs.loader.missing_imported_symbol` | A Lean/imported symbol is not supplied by the manifest dependency set. | Rebuild through `CargoLeanCapability` so dependency dylibs are recorded. |

Normal users should not repair shipped apps by setting `LD_LIBRARY_PATH` or `DYLD_*`. A missing search path in the
canonical flow is a packaging or artifact-manifest problem, not a caller-managed loader-flag problem.

## Worker bootstrap checks

Packaged worker applications can check deployment state before running a real command:

```rust
let report = builder.check();
if let Some(check) = report.first_error() {
    eprintln!("{}: {}", check.code(), check.repair_hint());
}
```

`LeanWorkerCapabilityBuilder::check()` validates the app-owned worker child, manifest-backed capability artifact,
protocol handshake, import session, and optional metadata expectation. It returns `LeanWorkerBootstrapReport`, not raw
child pids, pipe handles, protocol frames, loader flags, or `std::process` configuration.

| Worker bootstrap code | `as_str()` | Meaning | Common fix |
| --- | --- | --- | --- |
| `WorkerChildUnresolved` | `lean_rs.worker.bootstrap.child_unresolved` | The configured child locator did not resolve to a file. | Ship an app-owned worker child binary beside the application or set the documented worker-child override. |
| `WorkerChildNotExecutable` | `lean_rs.worker.bootstrap.child_not_executable` | The child path is missing or is not executable on this platform. | Build the worker child binary and package it with executable permissions. |
| `CapabilityPreflight { code: ... }` | `lean_rs.loader.*` | Manifest-backed loader preflight failed. | Follow the loader-code repair hint: rebuild through `CargoLeanCapability`, package missing artifacts, or use a compatible Lean toolchain. |
| `WorkerHandshakeFailed` | `lean_rs.worker.bootstrap.handshake_failed` | The child started but did not speak the worker protocol. | Ensure the binary calls `lean_rs_worker::run_worker_child_stdio()` and matches the parent crate version. |
| `CapabilityMetadataMismatch` | `lean_rs.worker.bootstrap.metadata_mismatch` | The capability metadata export did not match the caller's expectation. | Select or rebuild the intended capability package. |
| `WorkerStartupFailed` | `lean_rs.worker.bootstrap.startup_failed` | Startup failed outside a more specific bootstrap class. | Run the bootstrap check in the deployment environment and inspect the bounded message. |

Worker capability commands also use `lean_rs.worker.checked_binding` when a child session cannot resolve a requested
worker export through the manifest-backed closed operation table. The diagnostic names the requested export and the
operation shape, such as metadata `String -> IO String`, JSON command `String -> IO String`, or streaming command
`String, USize, USize -> IO UInt8`. Missing manifest identity, missing export metadata, and signature mismatch all fail
before invoking the Lean export. Rebuild through `CargoLeanCapability::export_signature(...)` or the worker builder's
metadata/doctor/command helpers so the artifact manifest carries the trusted worker signature.

`Internal` covers Rust callback panics caught before they unwind across C or Lean. It does not mean a Lean
kernel/runtime panic was contained. Those failures require a worker-process boundary.

Same-process callback shims also return `LeanCallbackStatus` for callback-local outcomes. `Ok = 0` continues the
Lean-side callback loop; `StaleHandle = 1` means Lean called a dropped handle; `Panic = 2` means Rust contained a
callback panic and recorded `LeanError::Host` on the live handle; `WrongPayload = 3` means Lean used a handle with the
wrong payload trampoline; and `Stopped = 4` means the Rust callback asked Lean to stop cleanly.

`lean-toolchain` uses a separate `LinkDiagnostics` enum for build-script work. Those diagnostics cover Lean discovery,
unsupported toolchains, missing `lake`, missing Lake targets, Lake build failures, and unresolved Lake outputs. They
render as one line so a `build.rs` can either return them or print them through `cargo:warning=`.

`Cancelled` is cooperative. It is returned only when `lean-rs-host` regains control and checks the token; it does not
pre-empt an in-flight Lean call. See
[`architecture/07-cooperative-cancellation.md`](architecture/07-cooperative-cancellation.md).

Progress sink panics are caught at the Rust callback boundary and surfaced as `LeanError::Host` with stage
`CallbackPanic`, code `lean_rs.internal`.

## Worker process diagnostics and pool snapshots

The worker crates deliberately use a separate worker error surface for process boundary failures. Child panic/abort,
request timeout, cancellation-triggered cycle, stale lease use, row sink panic, diagnostic sink panic, and typed command
decode failure are worker outcomes, not `LeanDiagnosticCode` values from the in-process service layer.

For large local runs, use `LeanWorkerPoolSnapshot` and `LeanWorkerSessionLease::snapshot()` for operational state.
Snapshots summarize queue depth, active workers, warm leases, restart reasons, best-effort child RSS, stream outcomes,
delivered row counts, payload bytes, elapsed stream time, and backpressure counters. They intentionally do not expose
child pids, worker ids, pipe handles, protocol frames, or scheduling internals.

## Matching on codes

`LeanError`, `LeanElabFailure`, and `LeanMetaResponse` all project to the same taxonomy via `.code()`:

```rust
use lean_rs::{LeanDiagnosticCode, LeanError};

fn report(err: &LeanError) {
    match err.code() {
        LeanDiagnosticCode::Linking => eprintln!("rebuild the capability: {err}"),
        LeanDiagnosticCode::ModuleInit => eprintln!("check `lake build` produced the dylib: {err}"),
        LeanDiagnosticCode::LeanException => {
            if let LeanError::LeanException(exc) = err {
                eprintln!("Lean raised {:?}: {}", exc.kind(), exc.message());
            }
        }
        LeanDiagnosticCode::Cancelled => eprintln!("caller cancelled the operation"),
        LeanDiagnosticCode::ResourceExhausted => eprintln!("configured resource budget exhausted: {err}"),
        other => eprintln!("unhandled {other}: {err}"),
    }
}
```

`LeanMetaResponse::code()` returns an `Option`: `None` on `Ok`, `Some(Unsupported)` when the capability lacked the
requested service, and `Some(Elaboration)` on the other two failure shapes (which carry a `LeanElabFailure`).
`LeanElabFailure::code()` always returns `Elaboration`.

## Tracing

`lean-rs` and `lean-rs-host` declare spans against the `lean_rs` target. Neither installs a subscriber—pick one
downstream, or use [`DiagnosticCapture`](#capturing-diagnostics-in-tests) for tests.

Recommended `RUST_LOG` scopes:

| Workload | `RUST_LOG` |
| --- | --- |
| Production default | `lean_rs=info,lean_toolchain=info` |
| Dev iteration | `lean_rs=debug,lean_toolchain=debug` |
| Full dispatch trace | `lean_rs=trace,lean_toolchain=trace` |

`info` covers init, library open, and session import (once-per-cycle events). `debug` adds per-session-method dispatch
(`query_declaration`, `elaborate`, `kernel_check`, bulk methods, pool acquire/release). `trace` adds per-dispatch
(`LeanExported::call`) and per-decoder events.

A typical `tracing-subscriber` wire-up:

```rust
tracing_subscriber::fmt()
    .with_env_filter(
        tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("lean_rs=info")),
    )
    .init();
```

### Spans emitted by `lean-rs`

FFI-primitive spans: init, library open, module initializer, typed-export dispatch, ABI decode. Fire whether the caller
is `lean-rs-host` or any other downstream of `lean-rs`.

| Span | Level | Fields |
| --- | --- | --- |
| `lean_rs.runtime.init` | info |—|
| `lean_rs.module.library.open` | debug | `path` (shortened) |
| `lean_rs.module.library.initialize` | debug | `package`, `module` |
| `lean_rs.module.initializer.call` | debug | `initializer` |
| `lean_rs.module.exported.call` | trace | `arity` |
| `lean_rs.abi.decode` (event) | trace | `shape`, `len` (bytes for strings; element count otherwise) |

### Spans emitted by `lean-rs-host`

Service-layer session and pool spans. Fire only if the caller opted into `lean-rs-host` and is driving a session.

| Span | Level | Fields |
| --- | --- | --- |
| `lean_rs.host.session.import` | info | `imports_len` (count of `&str` imports) |
| `lean_rs.host.session.query_declaration` | debug | `name` |
| `lean_rs.host.session.list_declarations` | debug |—|
| `lean_rs.host.session.list_declarations_filtered` | debug | `include_private`, `include_generated`, `include_internal` |
| `lean_rs.host.session.declaration_source_range` | debug | `name` |
| `lean_rs.host.session.declaration_type` / `_kind` / `_name` | debug | `name` |
| `lean_rs.host.session.declaration_type_bulk` / `_kind_bulk` / `_name_bulk` | debug | `batch_size` |
| `lean_rs.host.session.elaborate` | debug | `source_len` (chars), `heartbeats`, `diagnostic_byte_limit` |
| `lean_rs.host.session.kernel_check` | debug | `source_len`, `heartbeats`, `diagnostic_byte_limit` |
| `lean_rs.host.session.check_evidence` | debug |—|
| `lean_rs.host.session.summarize_evidence` | debug |—|
| `lean_rs.host.session.run_meta` | debug | `service` (name), `heartbeats`, `diagnostic_byte_limit` |
| `lean_rs.host.session.query_declarations_bulk` | debug | `batch_size` |
| `lean_rs.host.session.elaborate_bulk` | debug | `batch_size`, `heartbeats`, `diagnostic_byte_limit` |
| `lean_rs.host.pool.acquire` | debug | `imports_len` |
| `lean_rs.host.pool.acquire.result` (event) | debug | `hit` (bool) |
| `lean_rs.host.pool.release` (event) | trace | `kept` (bool) |

## Capturing diagnostics in tests

`DiagnosticCapture` is an in-process buffer for spans and events emitted against the `lean_rs` target. Thread-local and
bounded; the guard restores the previous subscriber on `Drop`.

```rust
use lean_rs::{DiagnosticCapture, LeanDiagnosticCode, LeanRuntime};
use lean_rs_host::LeanHost;

#[test]
fn rebuild_advice_fires_on_missing_dylib() {
    let capture = DiagnosticCapture::install();

    let runtime = LeanRuntime::init().unwrap();
    let host = LeanHost::from_lake_project(runtime, "/path/to/lake").unwrap();
    let err = host
        .load_capabilities("my_pkg", "DefinitelyMissingLib")
        .expect_err("missing dylib must fail");

    assert_eq!(err.code(), LeanDiagnosticCode::ModuleInit);

    let events = capture.events();
    assert!(events.iter().any(|e| e.span.as_deref()
        == Some("lean_rs.module.library.open")));
}
```

The guard is `!Send + !Sync`: it pins to the installing thread. Default capacity is 256 events; over-capacity drops the
oldest entry and bumps `capture.overflowed()`. Use `DiagnosticCapture::with_capacity(N)` for larger budgets. Scope is
just `lean_rs`; events from other crates pass through untouched.

## Redaction and bounding

Two values can grow without bound: Lean-authored text (capability messages, diagnostic messages) and filesystem paths.
Both are bounded at the construction site:

- **Lean-authored strings** pass through `bound_message` (the same 4 KiB cap that protects `LeanError::message`).
  Enforced on the UTF-8 char boundary, so multibyte sequences are never split.
- **Filesystem paths** emitted as span fields are shortened to the basename plus up to two parent components
  (`build/lib/lib.dylib`). The full absolute path is only emitted at `trace` level by call sites that explicitly need
  it.

Paths are not treated as secrets; shortening is a bounding decision, not a redaction decision. If a downstream policy
requires full path suppression, install a `tracing-subscriber` filter that drops the relevant fields.

## Cross-references

- [`lean_rs::LeanDiagnosticCode`](../crates/lean-rs/src/error/mod.rs)—the enum; `lean-rs` and `lean-rs-host` project to
  it.
- [`lean_rs::DiagnosticCapture`](../crates/lean-rs/src/error/capture.rs)—the in-process capture; captures spans from
  `lean-rs` and `lean-rs-host` against the shared `lean_rs` target.
- [Service-layer surface](architecture/03-host-stack.md)—methods on `LeanSession` and `SessionPool` that emit the
  service-layer spans above.
- [Concurrency contract](architecture/04-concurrency.md)—why spans are per-thread.
- [Safety model](architecture/01-safety-model.md)—why messages are bounded at construction.
- [Panic containment](architecture/06-panic-containment.md)—why Lean internal panics are process-scoped.
- [Cooperative cancellation](architecture/07-cooperative-cancellation.md)—where cancellation tokens are checked and what
  they cannot interrupt.
- [Callback registry](architecture/10-callback-registry.md)—how same-process callback statuses relate to the error
  taxonomy.
- [Interop build and link](architecture/12-interop-build-and-link.md)—typed `lean-toolchain` build diagnostics and cache
  behavior.
- [Structured progress](architecture/13-structured-progress.md)—live in-process progress events for long-running host
  calls.
