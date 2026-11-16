# Worker Session Adapter

The worker crates expose a narrow host-session adapter over the process boundary. It is not a remote `LeanSession`. The
in-process session remains the richer API for trusted callers; the worker adapter serves production callers that need
crash isolation, memory cycling, and typed IPC.

## Public Shape

Callers open a session with `LeanWorker::open_session` and a `LeanWorkerSessionConfig` naming the Lake project, package,
library, and import list. The returned `LeanWorkerSession` borrows the supervisor and exposes the supported
cross-process subset:

- `elaborate` returns `LeanWorkerElabResult`;
- `kernel_check` returns `LeanWorkerKernelResult`;
- `declaration_kinds` returns `Vec<String>`;
- `declaration_names` returns `Vec<String>`.

These outputs are deliberately serializable. The worker does not send `LeanExpr`, `LeanEvidence`, `LeanDeclaration`, or
`LeanName` handles to the parent because those handles are tied to the child Lean runtime. Diagnostics are copied into
`LeanWorkerDiagnostic` values with severity, message, file label, and optional source positions.

## Hidden Boundary

The private protocol carries frames for opening a host session and for the supported operations. That protocol is
versioned and tested, but it is not the caller-facing API. Callers do not manage `Command`, pipe reads, frame encoding,
child stderr parsing, or restart bookkeeping.

Inside the child, the adapter calls `lean-rs-host`. This keeps theorem-prover policy in `lean-rs-host` and process
policy in the worker crates.

## Progress And Cancellation

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

## Current Limits

The adapter intentionally does not mirror every `LeanSession` method. It does not support arbitrary capability
protocols, cross-process callback handles, `MetaM` services, evidence handles, source-range handles, or expression
transport. Add operations only when a production worker use case needs a process-safe value shape.

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
`LeanWorkerHostHandle::set_host_runtime_expectation` installs the broker's one current contract on an existing actor
without replacing its worker. It validates any cached actual metadata before changing the configuration. Extra
compatible bindings do not invalidate the subset. The parent retains metadata only after successful validation and
clears it before every new session open and shutdown/replacement attempt; old metadata never authorizes a new
generation. Inspection itself does not consume the request-cycle budget. An old worker that lacks this extension cannot
satisfy an opted-in requirement and receives no body command through that session.

The broker may advertise its expected contract while cold. Actual loaded metadata exists only after a session has opened
and must be bound to that actor/generation when projected into an execution receipt. It does not prove a submitted
command ran, a child exited, or a proof succeeded.


## Shared artifact read custody

`LeanWorkerChild::shared_artifact_reads(true)` requires the actual child to hold shared artifact locks while it imports
or runs a host operation. Generic library configuration defaults to false; the server's main and semantic worker
builders opt in. The configured helper owns the one read/write admission algorithm. The actual worker receives shared
lock descriptors through a private Unix socket and makes them inheritable only in that controlled process. The broker
uses atomic close-on-exec file opens for its pure-Rust cache and index reads; native children use guardian-owned loans.
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
before acquiring another lock. A failed guardian exchange also requires retirement before another admission because the
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
completed body result remains known even if subsequent read-scope release fails: the server keeps the recorded result
and retires that owner. A stopped receipt remains bound to its own request and generation; it cannot settle a new
request, a replacement startup, or another semantic companion.
