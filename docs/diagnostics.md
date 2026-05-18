# Diagnostics and observability

`lean-rs` (the L1 typed-FFI primitive) and `lean-rs-host` (the L2
opinionated theorem-prover-host stack) report failures through one
shared typed error (`lean_rs::LeanError`), plus the host-stack's
typed diagnostic payload (`lean_rs_host::LeanElabFailure`) and typed
meta response (`lean_rs_host::meta::LeanMetaResponse`). Every error-
bearing surface — on either crate — projects to the same stable
[`lean_rs::LeanDiagnosticCode`] taxonomy and threads structured
events through the standard [`tracing`] crate. The combination gives
a downstream caller two things:

- a stable identifier (`err.code()`) to react by family, independent of
  internal stage tags that may grow new variants;
- visibility into where time is spent and where failures originate,
  without rebuilding the crates or installing patched code.

This document is the catalogue. The crates' rustdocs cover the type
shapes; this file covers the operator-facing concepts and the recipes
for using them. The diagnostic-code taxonomy is unified across both
crates (every failure on either side maps to one of the same nine
codes); the span catalogue is split by emitting crate so the layer
boundary is visible at the log line.

## Diagnostic codes

Nine codes cover the failure families a caller can sensibly react to:

| Code | `as_str()` | Meaning |
| --- | --- | --- |
| `RuntimeInit` | `lean_rs.runtime_init` | Lean runtime initialization failed (panic in `lean_initialize_*`, task-manager init failure, thread-attach floor). |
| `Linking` | `lean_rs.linking` | A linkable artefact was missing or mismatched: invalid Lake package/module identifier, missing initializer symbol, header-digest mismatch. |
| `ModuleInit` | `lean_rs.module_init` | A capability dylib could not be opened, parsed, or its root module initializer raised. Also: the Lake project root did not exist. |
| `SymbolLookup` | `lean_rs.symbol_lookup` | A function or global symbol was not present in the loaded dylib when a session call tried to resolve it (`dlsym` miss or arity mismatch). |
| `AbiConversion` | `lean_rs.abi_conversion` | An ABI conversion failed: wrong Lean kind for the requested Rust type, integer out of range, invalid UTF-8, or a queried declaration was missing from the environment. |
| `LeanException` | `lean_rs.lean_exception` | Lean raised through its `IO` error channel. Inspect `LeanException::kind()` for the `IO.Error` constructor. |
| `Elaboration` | `lean_rs.elaboration` | Term parsing or elaboration produced one or more diagnostics. The payload is a `LeanElabFailure` with typed diagnostics. |
| `Unsupported` | `lean_rs.unsupported` | The loaded capability does not expose the requested service — either the Lean shim returned `unsupported` or the optional symbol was absent at load time. |
| `Internal` | `lean_rs.internal` | A `pub(crate)` invariant tripped, or a callback panicked inside the safe boundary. Indicates a bug in `lean-rs`. |

The variant names and `as_str()` ids are stable across patch releases.
New variants may be added; the enum is `#[non_exhaustive]`.

## Matching on codes

`LeanError`, `LeanElabFailure`, and `LeanMetaResponse` all project to
the same taxonomy via `.code()`:

```rust
use lean_rs::{LeanDiagnosticCode, LeanError, LeanResult};

fn report(err: &LeanError) {
    match err.code() {
        LeanDiagnosticCode::Linking => {
            eprintln!("rebuild the capability: {err}");
        }
        LeanDiagnosticCode::ModuleInit => {
            eprintln!("check `lake build` produced the dylib: {err}");
        }
        LeanDiagnosticCode::LeanException => {
            // The IO.Error constructor is on the inner payload.
            if let LeanError::LeanException(exc) = err {
                eprintln!("Lean raised {:?}: {}", exc.kind(), exc.message());
            }
        }
        other => eprintln!("unhandled {other}: {err}"),
    }
}
```

`LeanMetaResponse::code()` returns an `Option`: `None` on `Ok`,
`Some(Unsupported)` when the capability lacked the requested service,
and `Some(Elaboration)` on the other two failure shapes (which carry a
`LeanElabFailure`). `LeanElabFailure::code()` always returns
`Elaboration`.

## Tracing quick start

Both crates declare spans against the `lean_rs` target. Neither crate
installs a subscriber — pick one downstream, or use the in-process
[`DiagnosticCapture`](#capturing-diagnostics-in-tests) for tests.

Recommended `RUST_LOG` scopes:

| Workload | `RUST_LOG` |
| --- | --- |
| Production default | `lean_rs=info,lean_toolchain=info` |
| Dev iteration | `lean_rs=debug,lean_toolchain=debug` |
| Full dispatch trace | `lean_rs=trace,lean_toolchain=trace` |

`info` covers init, library open, and session import — once-per-cycle
events you want by default. `debug` adds per-session-method dispatch
(`query_declaration`, `elaborate`, `kernel_check`, bulk methods, pool
acquire/release). `trace` adds per-dispatch (`LeanExported::call`) and
per-decoder (`Vec`, `String`, `ByteArray` from-Lean conversions)
events.

The single `lean_rs` `RUST_LOG` target covers both crates: spans from
both `lean-rs` and `lean-rs-host` use the same target so one
`tracing-subscriber` filter scope catches the full call cascade.

### Emitted by `lean-rs` (L1)

The FFI-primitive spans — init, library open, module initializer,
typed-export dispatch, and the ABI decoder events. These fire whether
the caller is `lean-rs-host` or any other downstream of `lean-rs`.

| Span | Level | Fields |
| --- | --- | --- |
| `lean_rs.runtime.init` | info | (none) |
| `lean_rs.module.library.open` | debug | `path` (shortened) |
| `lean_rs.module.library.initialize` | debug | `package`, `module` |
| `lean_rs.module.initializer.call` | debug | `initializer` |
| `lean_rs.module.exported.call` | trace | `arity` |
| `lean_rs.abi.decode` (event) | trace | `shape`, `len` |

### Emitted by `lean-rs-host` (L2)

The host-stack session and pool spans. These only fire if the caller
opted in to the L2 stack by depending on `lean-rs-host` and driving a
session.

| Span | Level | Fields |
| --- | --- | --- |
| `lean_rs.host.session.import` | info | `imports_len` |
| `lean_rs.host.session.query_declaration` | debug | `name` |
| `lean_rs.host.session.list_declarations` | debug | (none) |
| `lean_rs.host.session.declaration_type/_kind/_name` | debug | `name` |
| `lean_rs.host.session.elaborate` | debug | `source_len`, `heartbeats`, `diagnostic_byte_limit` |
| `lean_rs.host.session.kernel_check` | debug | `source_len`, `heartbeats`, `diagnostic_byte_limit` |
| `lean_rs.host.session.check_evidence` | debug | (none) |
| `lean_rs.host.session.summarize_evidence` | debug | (none) |
| `lean_rs.host.session.run_meta` | debug | `service`, `heartbeats`, `diagnostic_byte_limit` |
| `lean_rs.host.session.call_capability` | debug | `symbol`, `arity` |
| `lean_rs.host.session.query_declarations_bulk` | debug | `batch_size` |
| `lean_rs.host.session.elaborate_bulk` | debug | `batch_size`, `heartbeats`, `diagnostic_byte_limit` |
| `lean_rs.host.pool.acquire` | debug | `imports_len` |
| `lean_rs.host.pool.acquire.result` (event) | debug | `hit` |
| `lean_rs.host.pool.release` (event) | trace | `kept` |

A typical wire-up with `tracing-subscriber`:

```rust
tracing_subscriber::fmt()
    .with_env_filter(
        tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("lean_rs=info")),
    )
    .init();
```

Then drive `lean-rs` normally; spans appear on stdout/stderr per the
formatter you chose.

## Capturing diagnostics in tests

`DiagnosticCapture` is an always-present in-process buffer for spans
and events emitted against the `lean_rs` target. The buffer is
thread-local and bounded; the guard restores the previous subscriber
on `Drop`.

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

The guard is `!Send + !Sync`: it pins to the thread that installed it.
The default buffer holds 256 events; over-capacity events drop the
oldest entry and bump `capture.overflowed()`. Construct with
`DiagnosticCapture::with_capacity(N)` for larger budgets.

The capture's scope is just the `lean_rs` target — events from other
crates pass through untouched.

## Common failures and fixes

### `Linking` — invalid identifier or missing initializer

A Lake package/module name failed the `[A-Za-z_][A-Za-z0-9_]*`
alphabet check, or the dynamic loader could not find the mangled
initializer symbol in the dylib.

Common causes and fixes:

- The Lake module name passed to `LeanHost::load_capabilities` does
  not match what `lake build` actually emitted. Inspect
  `fixtures/lean/.lake/build/lib/` (or your project's equivalent) and
  align the `lib_name` argument.
- The capability shared object is from a different Lean toolchain
  than the one the running process was linked against. Rebuild with
  the same `lean-toolchain`.

### `ModuleInit` — dylib not openable

The dylib does not exist, has the wrong architecture, or its
dependencies cannot be resolved by the system dynamic loader.

Common causes and fixes:

- `lake build` has not been run since the last source change. Run it.
- A transitive shared dependency (e.g., a system library the
  capability links against) is missing from `DYLD_LIBRARY_PATH`
  (macOS) or `LD_LIBRARY_PATH` (Linux). Add the directory.
- The Lake project root passed to `LeanHost::from_lake_project` is
  wrong. Verify the path is the directory that contains the `lakefile.lean`
  (or `lakefile.toml`).

### `SymbolLookup` — capability missing the export

The session tried to resolve a capability function symbol that was
not present in the loaded dylib, or the resolved symbol was a Lean
nullary-constant global where a function was expected.

Common causes and fixes:

- The Lean module that exports the symbol was not included in the
  capability's `lean_lib`. Add it to the Lake `lib_name` glob.
- `@[export]` is missing on the Lean side.
- The session is calling a service exported with one arity from a
  Rust call site expecting another.

### `AbiConversion` — wrong shape on the FFI boundary

The Lean value returned across the FFI boundary did not decode into
the declared Rust type: wrong constructor tag, non-scalar `char`,
heap-MPZ `Nat` where a scalar was expected, invalid UTF-8 in a Lean
`String`, or a queried declaration name was missing from the imported
environment.

Common causes and fixes:

- For `query_declaration`: the name is misspelled or the module that
  defines it is missing from the session's import list. Compare
  against `session.list_declarations()`.
- For numeric overflow: widen the Rust target type (`u64` → `i128`)
  or split the call.
- For `String`/`ByteArray`: the producer wrote invalid bytes. Check
  the Lean side's encoder.

### `LeanException` — Lean threw via `IO`

A Lean export raised through its `IO` error channel. The payload is
a `LeanException` carrying the `IO.Error` constructor (`UserError`,
`NoSuchThing`, `PermissionDenied`, …) and a bounded message.

Common causes and fixes:

- Inspect `exc.kind()` to choose a recovery path. `UserError` usually
  carries a Lean-authored explanation in `exc.message()`.
- For `noFileOrDirectory`/`permissionDenied`: the Lean code is doing
  `IO.FS.*` on a path the process cannot reach. Adjust the path or
  the working directory.

### `Elaboration` — parse or type error

The elaborator reported one or more diagnostics for the supplied
source. The payload is a `LeanElabFailure` carrying typed
diagnostics and a `truncated` flag.

Common causes and fixes:

- Walk `failure.diagnostics()`: each entry carries a `severity`,
  bounded `message`, optional `position`, and `file_label`. Use the
  `position` to locate the source span; the message names the
  expected/actual types.
- If `failure.truncated() == true`, raise
  `LeanElabOptions::diagnostic_byte_limit` and rerun.

### `Unsupported` — capability lacks the meta service

The loaded capability does not export the requested `MetaM` service
(or the Lean shim returned `unsupported` for the request shape).

Common causes and fixes:

- Rebuild the capability with the missing shim. The fixture in this
  repo exports all three (`infer_type`, `whnf`, `heartbeat_burn`) so
  this code typically signals a missing capability rebuild.

### `RuntimeInit` — Lean runtime did not come up

`LeanRuntime::init` panicked or its underlying C call failed.

Common causes and fixes:

- The host process linked against two different Lean runtimes. Pin
  one `lean-toolchain` across the workspace.
- An earlier process on this thread crashed inside Lean and left
  thread-local state behind. Restart the process.

### `Internal` — bug in `lean-rs`

A `pub(crate)` invariant tripped, or a callback panicked inside the
safe boundary. File a bug; include the bounded message and the
`as_str()` id.

## Redaction and bounding policy

Two values can grow without bound: Lean-authored text (capability
messages, diagnostic messages) and filesystem paths. Both are bounded
at the construction site so unattended log destinations cannot fill
disk:

- Lean-authored strings pass through `bound_message` (the same 4 KiB
  cap that protects `LeanError::message`). The bound is enforced on
  the UTF-8 char boundary, so multibyte sequences are never split.
- Filesystem paths emitted as span fields are shortened to the
  basename plus up to two parent components (`build/lib/lib.dylib`).
  The full absolute path is only emitted on demand at `trace` level
  by call sites that explicitly need it.

Paths are not treated as secrets. The shortening is a *bounding*
decision, not a *redaction* decision: bounded values keep one span on
one terminal line. If a downstream policy requires full path
suppression, install a `tracing-subscriber` filter that drops the
relevant fields.

## Cross-references

- [`lean_rs::LeanDiagnosticCode`](../crates/lean-rs/src/error/mod.rs)
  — the enum, defined on the L1 crate; both crates project to it.
- [`lean_rs::DiagnosticCapture`](../crates/lean-rs/src/error/capture.rs)
  — the in-process capture, also on L1 (captures spans from both
  crates against the shared `lean_rs` target).
- [Host stack surface](architecture/04-host-stack.md) — the
  `lean-rs-host` crate's curated surface; spans listed under
  *Emitted by `lean-rs-host`* originate from the methods on
  `LeanSession` and `SessionPool` described there.
- [Concurrency contract](architecture/04-concurrency.md) — why
  spans are per-thread.
- [Safety model](architecture/01-safety-model.md) — why messages are
  bounded at construction.
