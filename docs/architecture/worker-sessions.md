# Worker Session Adapter

`LeanWorker::open_session` returns a borrowing `LeanWorkerSession`. Configure it with a Lake project and imports,
a manifest-backed capability, or `LeanWorkerSessionConfig::shims_only` for host services without a downstream
capability. The child performs Lean work; the parent receives copied, serializable results.

The worker records the effective ordered search roots during the first observed import for an admitted host anchor.
Before reusing a current or parked environment, it asks the same shim to compute the requested order under the
current import admission. A changed order retires the child before the request body. A legacy shim can open fresh
sessions. The ordered paths say where Lean searched. The application still owns stability of the files and of
earlier module prefixes that could shadow them. Relative entries use the worker’s working directory
at import and comparison time. Before each source grant, `WorkerAuthority::validate_source_context` receives
the opened session context and its retained root order. Applications that require this observation can refuse a
legacy session with no recorded order.

An application can select one native plugin in its unsafe import attestation. The application verifies the canonical
image, explicit initializer, matching compiled modules and dependencies, and keeps them unchanged while the worker
runs. It establishes the loader environment before executing the worker or its helpers. Ordinary requests supply only the
existing import fields. Lean loads the plugin during the import transaction and can retain it if the import fails.
The worker binds the policy on its first import attempt. Changing the image, initializer or presence of the plugin
requires a fresh worker. Plugin-free imports continue to work with older shims. A plugin requires the versioned
plugin entry and supports ordinary session import and path comparison.

## Supported operations

The adapter exposes bounded service operations rather than runtime-bound handles:

| Area | Methods |
| --- | --- |
| Elaboration and kernel checking | `elaborate`, `kernel_check` |
| Meta operations | `infer_type`, `whnf`, `is_def_eq` |
| Declarations | `declaration_kinds`, `declaration_names`, `list_declarations_strings`, `describe`, `describe_bulk`, `search_declarations`, `declaration_type`, `inspect_declaration` |
| Source queries | `process_module_query`, `process_module_query_batch`, `clear_module_snapshot_cache` |
| Proof work | `attempt_proof`, `verify_declaration`, `verify_declaration_batch` |
| Downstream capabilities | Typed JSON and streaming commands, capability metadata and doctor checks |
| Loaded host contract | `host_runtime_metadata` |

`LeanExpr`, `LeanEvidence`, `LeanDeclaration`, and `LeanName` handles remain inside the child. Meta operations return
rendered values or booleans with structured status. Each meta pass, including notation-aware pretty-printing, has its
own heartbeat limit. Only an unsupported pretty-printer falls back to raw rendering; a pretty-printer failure fails
the whole call. Proof attempts use in-memory source overlays and never write source files.

`LeanWorkerDiagnostic` copies severity, message, file label, and available source positions. Ordinary source diagnostics
use `original_source` and may carry `original_range`. Proof-overlay diagnostics use `synthetic_buffer`: their line and
column refer to generated text and must not be used as editable positions in the original file. Missing coordinate
metadata from older peers decodes as `unknown`.

Proof-state results include bounded source-order proof boundaries for retrying a selector when an `after_text` or
default position cannot be resolved. These are recovery points, not a general source map; their cap is independent of
the diagnostic cap. For example:

```json
{
  "diagnostics": [{
    "severity": "error",
    "message": "unknown identifier 'missingIdentifier'",
    "line": 2,
    "column": 9,
    "coordinate_space": "synthetic_buffer",
    "original_range": null
  }],
  "proof_boundaries": [{
    "index": 0,
    "kind": "entry",
    "source": { "start_line": 2, "start_column": 3, "end_line": 2, "end_column": 10 },
    "excerpt": { "value": "exact h", "truncated": false }
  }],
  "proof_boundaries_truncated": false
}
```

## Protocol boundary

The supervisor hides pipe reads, frame encoding, stderr parsing, and restart bookkeeping from session callers.
`lean-rs-worker-protocol` separately publishes the versioned codec and wire types for alternative peers, recorders, and
harnesses; ordinary host calls do not require that interface.

Inside the child, the adapter calls `lean-rs-host`. This keeps theorem-prover policy in `lean-rs-host` and process
policy in the worker crates.

## Progress and cancellation

Worker progress is a parent-side IPC event, not a cross-process callback handle. `LeanWorkerProgressSink` receives
`LeanWorkerProgressEvent` values with a string phase, current count, optional total, and elapsed duration measured in
the parent.

`LeanWorkerCancellationToken` is also parent-side. The supervisor checks it before sending a request and after progress
frames while a request is in flight, and at intervals of at most 50 ms while waiting for IPC even if the child is
silent. The observation bound excludes time spent in caller-provided sinks and OS scheduling. Cancellation completes
through the normal kill, reap, reader-drain, and replacement path; that cleanup adds its own bounded deadlines. If
cancellation is observed during a request, the supervisor cycles the child process, returns
`LeanWorkerError::Cancelled`, and invalidates the open `LeanWorkerSession`. Open a new session before issuing more host
requests.

If replacement startup fails, a previous child's exit does not acknowledge the failed replacement's exit. The supervisor
retains any replacement whose cleanup remains unconfirmed and rejects status and new requests until structured shutdown
can establish exit. This ownership rule applies to every replacement policy; callers do not need to infer it from
cancellation, timeout, or setup error strings.

This is a worker-safe boundary. It does not share an in-process `LeanCancellationToken` or `LeanCallbackHandle` with the
child.

## Extension boundary

New host operations need a process-safe result shape and explicit output/resource budgets. Runtime handles,
cross-process callback handles, and arbitrary access to child-local `MetaM` state are outside this adapter. Downstream
exports use the [capability layer](worker-capabilities.md).

## Loaded host-runtime metadata

`LeanWorkerSession::host_runtime_metadata` is an opt-in, metadata-only worker request. It requires an already-open host
session and works for shims-only sessions; it never calls a downstream capability export or opens/imports a session
itself. The child reports only signatures represented by successfully manifest-checked typed `HostShimBindings`, with
optional absent symbols omitted. It reports the running child's compiled Lean version/header separately from an optional
Lean version read through the loaded `lean_rs_host_runtime_version` shim. This remains the cooperative library trust
boundary, not a certificate against library substitution or hostile metaprograms.

`LeanWorkerHostHandleBuilder::require_host_runtime`, `LeanWorkerCapabilityBuilder::require_host_runtime`, and
`LeanWorkerSessionConfig::with_host_runtime_expectation` enforce
a required subset before exposing each host session, including session opens after an internal recycle.
Capability builders also include the requirement in their pool reuse key, so an existing unguarded or differently
constrained session cannot satisfy a newly required contract.
`LeanWorkerHostHandle::set_host_runtime_expectation` installs a contract on a retained host handle
without replacing its worker. It validates any cached actual metadata before changing the configuration. Extra
compatible bindings do not invalidate the subset. The parent retains metadata only after successful validation and
clears it before every new session open and shutdown/replacement attempt; old metadata never authorizes a new
generation. Inspection itself does not consume the request-cycle budget. An old worker that lacks this extension cannot
satisfy an opted-in requirement and receives no body command through that session.

An embedder may advertise its expected contract before opening a session. Actual loaded metadata exists only after a
session has opened and must be bound to that owner/generation when projected into an execution receipt. It does not
prove a submitted command ran, a child exited, or a proof succeeded.

## Prepared host artifacts

An application with an immutable prepared deployment can pass its descriptor through
`LeanWorkerChild::prepared_host_artifacts` to an ordinary child running `run_worker_child_with_authority`.
The child checks the bounded descriptor and named shim manifest, then uses those inputs for every host anchor in
that generation. Only an omitted ordinary descriptor selects bundled shim discovery and preparation. An invalid
explicit descriptor or a refused grant fails the operation. The application's `WorkerAuthority::host` must
independently verify the exact manifest, images, initializers and ordered import paths. The launch-time byte comparison
alone cannot establish immutability. Project manifest reads, native loads and imports still require their ordinary
application grants, including `use_generated_project_imports` to select generated project output.

The worker binary has runtime-free preparation entries. `--lean-rs-ready-host-artifacts` checks the existing
host/interop build keys and the exact manifests without materializing, building, loading or publishing anything, and
`--lean-rs-ready-host-artifacts-generated KEY` checks one generated output slot; a miss exits 75.
`--lean-rs-prepare-host-artifacts [DEST]` builds the shims and emits one bounded storage descriptor before any
`LeanRuntime::init` or parent lifeline. Without `DEST` it builds the shims where Cargo unpacked `lean-rs-host`. With
`DEST`, a directory that must not exist yet, it first writes the shim sources compiled into the worker there and builds
them there, so an installer can give a worker its own copy (`lean_rs_host::host::prepare_host_artifacts_in`). The parent
passes that exact descriptor to a native generation. The descriptor is planned storage metadata, never a loaded-runtime
receipt.

The SDK takes no lock on these artifacts or on project outputs. The process that owns the checkout stops or cycles the
workers that read a project before it builds that project, so no session imports some modules from before a build and
some from after.

## Terminal errors and lifecycle evidence

A typed terminal error from a healthy, non-retiring owned worker proves that operation returned. It does not prove a
successful proof, or require terminating an otherwise usable worker. Exact checkpoint-bound request metadata identifies
whether the failure occurred during session import or runtime validation before a named target body. A later generation
change does not by itself make this returned outcome unknown.

Unconfirmed transport, shutdown or descendant cleanup retains its exact pending exit observation and capacity fence. A
stopped receipt remains bound to its own request and generation; it cannot settle a new request, a replacement startup,
or another worker.
