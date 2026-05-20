# Worker Data Streaming

`lean-rs-worker` needs a row stream for downstream protocols that produce
arbitrary user data while a worker request is running. The host-session adapter
can already return copied theorem-prover values and progress events, but that is
not enough for tools that need a sequence of downstream-owned rows such as
JSONL-like `lean-dup` output.

Worker data rows are worker IPC data. They are not L1 callback payloads and not
Lean runtime handles. The worker owns process framing, ordering, cancellation
boundaries, fatal-exit behavior, and sink panic containment. Downstream crates
own the row schema.

## Public Row Shape

The selected public row type is:

```rust
pub struct LeanWorkerDataRow {
    pub stream: String,
    pub sequence: u64,
    pub payload: serde_json::Value,
}
```

`stream` is a caller-defined channel name, such as `"rows"`, `"warnings"`, or a
tool-specific stream label. `sequence` is assigned by `lean-rs-worker` per
stream within one request. `payload` is arbitrary JSON owned by the downstream
protocol.

This type is intentionally generic at the worker boundary and intentionally not
schema-free inside downstream tools. `lean-rs-worker` carries rows. It does not
define `lean-dup` row structs, theorem-search result schemas, or application
business objects.

## Chosen Boundary

`lean-rs-worker` owns:

- length-delimited, versioned row frames in the private worker protocol;
- per-stream row ordering;
- conversion from child-side streaming callbacks to parent-side row events;
- row-boundary cancellation behavior;
- EOF and fatal-child-exit behavior while a stream is active;
- containment of parent-side data-sink panics.

Downstream crates own:

- stream names and their meanings;
- JSON object shape;
- validation beyond "is valid JSON";
- semantic interpretation of each row;
- persistence, indexing, deduplication, and UI policy.

The boundary follows the same rule as the worker-process boundary: the worker
hides process and IPC mechanics, but it does not absorb downstream domain
schemas.

## Rejected Designs

**Raw stdout JSONL.** Rejected. It exposes process I/O, framing, completion,
EOF, cancellation, and fatal-exit rules to every consumer. It also competes with
structured worker diagnostics and progress for the same untyped channel. That
would make each downstream tool reimplement the worker protocol outside the
worker.

**Public cross-process callbacks.** Rejected. `LeanCallbackHandle` is an
in-process L1 mechanism. Its handle lifetime, trampoline values, refcount rules,
panic containment, and wrong-payload checks are valid only inside one process.
Passing those handles across the worker boundary would turn an in-process FFI
tool into an IPC API and leak exactly the mechanics the worker exists to hide.

**Worker data rows.** Chosen. A worker row stream is the smallest public shape
that supports arbitrary downstream JSON rows while preserving the worker's deep
module boundary. Callers learn a row sink and a row type; they do not learn frame
bytes, pipe reads, child exits, or callback handles.

## Relationship To L1 Callback Payloads

The child may use in-process L1 callbacks to collect strings from a Lean export,
but those callbacks remain child-local. For the first streaming runner, the
child registers a `LeanCallbackHandle<LeanStringEvent>`, parses each callback
string as one JSON row, and sends a private `DataRow` frame to the parent.

Byte streaming and Lean-object callbacks stay in the L1 callback-payload track.
They require their own ABI and soundness work because they change how Lean data
enters Rust in one process. Worker row streaming does not shortcut that work:
it serializes process-safe JSON values over IPC.

## Prompt 60 Replan

Prompt 60 originally targeted worker recipes and `lean-dup` readiness after the
host-session adapter. That was too early. The adapter can return host-session
responses, progress, and diagnostics, but it cannot yet carry arbitrary
downstream row streams.

Prompt 60 is therefore replanned. The executable sequence is:

1. Add private `DataRow` protocol frames.
2. Add the public row sink API.
3. Add a streaming capability runner that turns child-local string callbacks
   into worker data rows.
4. Add worker recipes and the `lean-dup` readiness proof on top of those row
   contracts.

This keeps the recipe honest: it demonstrates the production worker stream
boundary instead of falling back to raw stdout JSONL.

## Consumer Guidance

Use direct L1 string callbacks when the Lean extension is trusted, in-process,
and the application does not need crash isolation or memory reset.

Use worker data rows when the application needs the worker process boundary.
The row payload can be any JSON value, but `lean-rs-worker` treats it as data.
Schema ownership remains with the downstream crate.
