# Diagnostics and Observability

Both crates project errors to the same stable [`lean_rs::LeanDiagnosticCode`] taxonomy and
emit structured `tracing` spans against the `lean_rs` target. A downstream caller gets:

- a stable identifier (`err.code()`) to react by family, independent of internal stage tags that may grow new variants;
- visibility into where time is spent and where failures originate, without rebuilding the crates.

The taxonomy is unified across both crates (every recoverable failure on either side maps to
a stable code); the span catalogue is split by emitting crate so the layer boundary is
visible at the log line. Lean internal panics are outside this error taxonomy: there is no
`SessionPoisoned` code. A Lean runtime panic during a `LeanSession` call may terminate the
process; see [`architecture/06-panic-containment.md`](architecture/06-panic-containment.md).

## Diagnostic codes

| Code | `as_str()` | Meaning | Common fix |
| --- | --- | --- | --- |
| `RuntimeInit` | `lean_rs.runtime_init` | Lean runtime init failed (panic in `lean_initialize_*`, task-manager failure, thread-attach floor). | Pin one `lean-toolchain` across the workspace. Restart the process if an earlier crash left thread-local state behind. |
| `Linking` | `lean_rs.linking` | A linkable artefact was missing or mismatched: invalid Lake package/module identifier, missing initializer symbol, header-digest mismatch. | Verify the `lib_name` matches what `lake build` emitted (inspect `.lake/build/lib/`). Rebuild the capability against the same `lean-toolchain` as the host process. |
| `ModuleInit` | `lean_rs.module_init` | A capability dylib could not be opened, parsed, or its root initializer raised. Includes Lake project root not existing. | Re-run `lake build`. Add the directory of any missing transitive shared dependency to `DYLD_LIBRARY_PATH` (macOS) / `LD_LIBRARY_PATH` (Linux). Verify the path passed to `LeanHost::from_lake_project` contains the `lakefile.lean` / `lakefile.toml`. |
| `SymbolLookup` | `lean_rs.symbol_lookup` | A function or global symbol was not present in the loaded dylib (`dlsym` miss or arity mismatch). | Add the Lean module exporting the symbol to the capability's `lean_lib`. Check `@[export]` on the Lean side. Check that the Rust call site's expected arity matches the export. |
| `AbiConversion` | `lean_rs.abi_conversion` | Wrong Lean kind for the requested Rust type, integer out of range, invalid UTF-8, or a queried declaration was missing from the environment. | For `query_declaration`: compare against `session.list_declarations(None)`. For numeric overflow: widen the Rust target type or split the call. For `String`/`ByteArray`: check the Lean encoder. |
| `LeanException` | `lean_rs.lean_exception` | Lean raised through its `IO` error channel. Inspect `LeanException::kind()` for the `IO.Error` constructor. | Branch on `exc.kind()`. For `noFileOrDirectory` / `permissionDenied`, adjust the path or working directory of the Lean code. |
| `Elaboration` | `lean_rs.elaboration` | Term parsing or elaboration produced diagnostics. Payload is `LeanElabFailure` with typed diagnostics. | Walk `failure.diagnostics()`: each carries a `severity`, bounded `message`, optional `position`, and `file_label`. If `failure.truncated() == true`, raise `LeanElabOptions::diagnostic_byte_limit` and rerun. |
| `Unsupported` | `lean_rs.unsupported` | Loaded capability lacks the requested service (Lean shim returned `unsupported` or the optional symbol was absent at load). | Rebuild the capability with the missing shim. The fixture in this repo exports all four meta services (`infer_type`, `whnf`, `heartbeat_burn`, `is_def_eq`); this code typically signals a missing rebuild. |
| `Cancelled` | `lean_rs.cancelled` | A `lean-rs-host` cooperative cancellation token was observed before a host-controlled FFI dispatch. | Treat the operation as aborted and discard partial work. Create a fresh token before retrying. |
| `Internal` | `lean_rs.internal` | A `pub(crate)` invariant tripped, or a callback panicked inside the safe boundary. | File a bug; include the bounded message and the `as_str()` id. |

The enum is `#[non_exhaustive]`; new variants may be added. Variant names and `as_str()` ids
are stable across patch releases.

`Internal` covers Rust callback panics caught before they unwind across C or Lean. It does not
mean a Lean kernel/runtime panic was contained. Those failures require a worker-process
boundary.

`Cancelled` is cooperative. It is returned only when `lean-rs-host` regains
control and checks the token; it does not pre-empt an in-flight Lean call.
See [`architecture/07-cooperative-cancellation.md`](architecture/07-cooperative-cancellation.md).

## Matching on codes

`LeanError`, `LeanElabFailure`, and `LeanMetaResponse` all project to the same taxonomy via
`.code()`:

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
        other => eprintln!("unhandled {other}: {err}"),
    }
}
```

`LeanMetaResponse::code()` returns an `Option`: `None` on `Ok`, `Some(Unsupported)` when the
capability lacked the requested service, and `Some(Elaboration)` on the other two failure
shapes (which carry a `LeanElabFailure`). `LeanElabFailure::code()` always returns
`Elaboration`.

## Tracing

Both crates declare spans against the `lean_rs` target. Neither installs a subscriber—pick
one downstream, or use [`DiagnosticCapture`](#capturing-diagnostics-in-tests) for tests.

Recommended `RUST_LOG` scopes:

| Workload | `RUST_LOG` |
| --- | --- |
| Production default | `lean_rs=info,lean_toolchain=info` |
| Dev iteration | `lean_rs=debug,lean_toolchain=debug` |
| Full dispatch trace | `lean_rs=trace,lean_toolchain=trace` |

`info` covers init, library open, and session import (once-per-cycle events). `debug` adds
per-session-method dispatch (`query_declaration`, `elaborate`, `kernel_check`, bulk methods,
pool acquire/release). `trace` adds per-dispatch (`LeanExported::call`) and per-decoder events.

A typical `tracing-subscriber` wire-up:

```rust
tracing_subscriber::fmt()
    .with_env_filter(
        tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("lean_rs=info")),
    )
    .init();
```

### Spans emitted by `lean-rs` (L1)

FFI-primitive spans: init, library open, module initializer, typed-export dispatch, ABI decode.
Fire whether the caller is `lean-rs-host` or any other downstream of `lean-rs`.

| Span | Level | Fields |
| --- | --- | --- |
| `lean_rs.runtime.init` | info |—|
| `lean_rs.module.library.open` | debug | `path` (shortened) |
| `lean_rs.module.library.initialize` | debug | `package`, `module` |
| `lean_rs.module.initializer.call` | debug | `initializer` |
| `lean_rs.module.exported.call` | trace | `arity` |
| `lean_rs.abi.decode` (event) | trace | `shape`, `len` (bytes for strings; element count otherwise) |

### Spans emitted by `lean-rs-host` (L2)

Host-stack session and pool spans. Fire only if the caller opted into the L2 stack and is
driving a session.

| Span | Level | Fields |
| --- | --- | --- |
| `lean_rs.host.session.import` | info | `imports_len` (count of `&str` imports) |
| `lean_rs.host.session.query_declaration` | debug | `name` |
| `lean_rs.host.session.list_declarations` | debug |—|
| `lean_rs.host.session.declaration_type` / `_kind` / `_name` | debug | `name` |
| `lean_rs.host.session.elaborate` | debug | `source_len` (chars), `heartbeats`, `diagnostic_byte_limit` |
| `lean_rs.host.session.kernel_check` | debug | `source_len`, `heartbeats`, `diagnostic_byte_limit` |
| `lean_rs.host.session.check_evidence` | debug |—|
| `lean_rs.host.session.summarize_evidence` | debug |—|
| `lean_rs.host.session.run_meta` | debug | `service` (name), `heartbeats`, `diagnostic_byte_limit` |
| `lean_rs.host.session.call_capability` | debug | `symbol`, `arity` |
| `lean_rs.host.session.query_declarations_bulk` | debug | `batch_size` |
| `lean_rs.host.session.elaborate_bulk` | debug | `batch_size`, `heartbeats`, `diagnostic_byte_limit` |
| `lean_rs.host.pool.acquire` | debug | `imports_len` |
| `lean_rs.host.pool.acquire.result` (event) | debug | `hit` (bool) |
| `lean_rs.host.pool.release` (event) | trace | `kept` (bool) |

## Capturing diagnostics in tests

`DiagnosticCapture` is an in-process buffer for spans and events emitted against the `lean_rs`
target. Thread-local and bounded; the guard restores the previous subscriber on `Drop`.

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

The guard is `!Send + !Sync`: it pins to the installing thread. Default capacity is 256 events;
over-capacity drops the oldest entry and bumps `capture.overflowed()`. Use
`DiagnosticCapture::with_capacity(N)` for larger budgets. Scope is just `lean_rs`; events from
other crates pass through untouched.

## Redaction and bounding

Two values can grow without bound: Lean-authored text (capability messages, diagnostic
messages) and filesystem paths. Both are bounded at the construction site:

- **Lean-authored strings** pass through `bound_message` (the same 4 KiB cap that protects `LeanError::message`). Enforced on the UTF-8 char boundary, so multibyte sequences are never split.
- **Filesystem paths** emitted as span fields are shortened to the basename plus up to two parent components (`build/lib/lib.dylib`). The full absolute path is only emitted at `trace` level by call sites that explicitly need it.

Paths are not treated as secrets; shortening is a bounding decision, not a redaction decision.
If a downstream policy requires full path suppression, install a `tracing-subscriber` filter
that drops the relevant fields.

## Cross-references

- [`lean_rs::LeanDiagnosticCode`](../crates/lean-rs/src/error/mod.rs)—the enum; defined on L1, both crates project to it.
- [`lean_rs::DiagnosticCapture`](../crates/lean-rs/src/error/capture.rs)—the in-process capture; captures spans from both crates against the shared `lean_rs` target.
- [Host stack surface](architecture/04-host-stack.md)—methods on `LeanSession` and `SessionPool` that emit the L2 spans above.
- [Concurrency contract](architecture/04-concurrency.md)—why spans are per-thread.
- [Safety model](architecture/01-safety-model.md)—why messages are bounded at construction.
- [Panic containment](architecture/06-panic-containment.md)—why Lean internal panics are process-scoped.
- [Cooperative cancellation](architecture/07-cooperative-cancellation.md)—where cancellation tokens are checked and what they cannot interrupt.
