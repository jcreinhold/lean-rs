# Worker Session Adapter

`LeanWorker::open_session` returns a borrowing `LeanWorkerSession`. Configure it with a Lake project and imports,
a manifest-backed capability, or `LeanWorkerSessionConfig::shims_only` for host services without a downstream
capability. The child performs Lean work; the parent receives copied, serializable results.

The worker records the effective ordered search roots during the first observed import for an admitted host anchor.
Before reusing a current or parked environment, it asks the same shim to compute the requested order under the
current import admission. A changed order retires the child before the request body. A legacy shim can open fresh
sessions. The ordered paths say where Lean searched. The application and artifact readers still own stability of
the files and of earlier module prefixes that could shadow them. Relative entries use the worker’s working directory
at import and comparison time.

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

A guarded worker can report `effective_import_roots` alongside `artifact_read_epochs`. The list preserves the order
and duplicates observed during the active session’s import. Relative entries, including empty entries, use the
worker’s import-time working directory. The worker omits the list if it cannot give absolute paths, if the shim
did not observe the order, if there is no active read custody, or if the complete list exceeds the frame budget.
Ordinary execution still works. Consumers using these facts for reuse must bind them to the same session and epochs
and establish custody for the artifacts and shadowing prefixes they depend on.

Older readers ignore this optional field. Older workers omit it. When updating a Rust struct literal for
`LeanWorkerHostRuntimeMetadata`, add `effective_import_roots: None` for an unobserved order.

An embedder may advertise its expected contract before opening a session. Actual loaded metadata exists only after a
session has opened and must be bound to that owner/generation when projected into an execution receipt. It does not
prove a submitted command ran, a child exited, or a proof succeeded.

## Shared artifact read custody

`LeanWorkerChild::shared_artifact_reads(true)` requires the actual child to hold shared artifact locks while it imports
or runs a host operation. Generic library configuration defaults to false; embedders opt in on each worker builder
that requires this coordination. The configured helper owns the read/write admission algorithm. The actual worker
receives shared lock descriptors through a private Unix socket and makes them inheritable only in that controlled process. Native
children use guardian-owned loans; broker-side Rust cache/index readers use atomic close-on-exec opens.
Multithreaded embedders may instead use the helper proxy, without receiving a lock descriptor. Canonical sibling lock
files also admit a genuinely cold root without creating `.lake`; an existing `.lake` must remain an unaliased directory
in that root.

The worker binary has two runtime-free preparation entries. `--lean-rs-ready-host-artifacts` takes shared helper custody
and checks existing host/interop build keys and the exact manifests without materializing, building, loading or
publishing anything; a miss exits 75. `--lean-rs-prepare-host-artifacts` uses only the configured writer helper and
emits one bounded storage descriptor before any `LeanRuntime::init`, native lifeline or guardian bootstrap. The parent
passes that exact descriptor to a guarded native generation. The descriptor is planned storage metadata, never a
loaded-runtime receipt.

Before any capability `dlopen`, module initializer, import or body, the guarded child independently reconstructs the
canonical union from the shim snapshot, optional capability snapshot and import workspace's complete Lake dependency
graph. It requires the descriptor's shim union to agree, acquires all shared domains in the same hashed lock-path order
as exclusive writers, and waits for the independent guardian's durable FD-custody acknowledgement. It then rechecks the
captured manifest bytes and workspace graph before consuming exactly those snapshots. Missing, changed or unsupported
metadata refuses before native loading. Guarded workers reject the direct fixture-manifest loaders even when another
host read scope is active.

The generation retains one fixed read union. A later session requiring different domains requests a fresh generation
before acquiring another lock. Cold admission can wait on the complete fixed domain union only after every partial
read has closed. Warm admission makes one pass: typed contention maps to the existing pre-body epoch-change response,
so the parent confirms full generation closure before its single fresh open attempt. This releases the guardian's old
SH union; dropping only the new local guards would not. The outer request identity remains stable and its target body
is invoked once. Hard errors and any failure after inheritance or guardian exchange are never retried by this loop.
A failed guardian exchange also requires retirement before another admission because the
guardian may already have retained custody. Loaded capabilities use ready import paths; no native-generation import or
body may trigger a nested Lake writer. The parent probes warm artifacts under shared custody before requesting exclusive
preparation, and drains affected generations before a genuine writer.

Every genuine writer advances its output epoch before starting work, including work that later fails. The worker
compares all held read epochs before reusing imported state. A change requests a full child cycle before any target
body, invalidating process-global Lean state and every pooled environment. Comparing only the named import's `.olean` is
insufficient when a failed build changed a transitive import. The broker must independently compare the same epoch
before consulting its own result cache.

The read scope spans `OpenHostSession`, loaded metadata validation and the subsequent host body commands.
`ReleaseHostRead` closes this child's request aliases after a known terminal operation; it never explicitly unlocks an
open file description. The independent guardian retains the generation's complete FD union until that owned process
group is confirmed empty, including descendants that closed their inherited aliases. A known import refusal closes
aliases before returning. An unknown in-flight operation cannot release custody and must retire through the supervisor.
Embedders that enable the option finish a known scope with `LeanWorker::release_host_read` or its host/capability
delegation after dropping the session borrow; dropping `LeanWorkerSession` alone does not stop the worker or finish the
scope.

`LeanWorkerHostRuntimeMetadata::artifact_read_epochs` records the actual held domains and epochs at session validation.
It is absent in an older or unconfigured worker. A read-required parent refuses that absence before exposing a body
session. A receipt preserves past execution evidence; it is not a claim that the worker remains alive or still holds
those descriptors when the receipt is read. Children that deliberately leave the ownership scope and close inherited
custody are outside this cooperative native-read contract.

## Terminal errors and lifecycle evidence

A typed terminal error from a healthy, non-retiring owned worker proves that operation returned. It does not prove a
successful proof, or require terminating an otherwise usable worker. Exact checkpoint-bound request metadata identifies
whether the failure occurred during session import or runtime validation before a named target body. A later generation
change does not by itself make this returned outcome unknown.

Unconfirmed transport, shutdown or descendant cleanup retains its exact pending exit observation and capacity fence. A
completed body result remains known even if subsequent read-scope release fails: an embedder preserves the recorded
result and retires that owner. A stopped receipt remains bound to its own request and generation; it cannot settle a new
request, a replacement startup, or another semantic companion.
