# Cooperative Cancellation

`lean-rs-host` cancellation is cooperative. A caller creates a
`LeanCancellationToken`, shares a clone with the thread that may request
cancellation, and passes `Some(&token)` to a `LeanSession` operation. The
operation checks the token before it enters Lean and, for token-aware bulk
paths, between per-item dispatches. If the token has been cancelled, the
operation returns `LeanError::Cancelled` with diagnostic code
`lean_rs.cancelled`.

```rust
use std::thread;
use lean_rs_host::LeanCancellationToken;

let token = LeanCancellationToken::new();
let canceller = token.clone();

thread::spawn(move || {
    canceller.cancel();
});

// On the Lean worker thread:
// session.query_declarations_bulk(&names, Some(&token))?;
```

Already-collected bulk results are discarded. There is no partial-output
return shape.

## What Is Checked

Every host operation that can enter Lean accepts
`Option<&LeanCancellationToken>` as its final argument:

- `LeanCapabilities::session`
- `SessionPool::acquire`
- `LeanSession::{query_declaration,list_declarations,declaration_type,declaration_kind,declaration_name}`
- `LeanSession::{elaborate,kernel_check,check_evidence,summarize_evidence}`
- `LeanSession::{run_meta,query_declarations_bulk,declaration_type_bulk,declaration_kind_bulk,declaration_name_bulk,elaborate_bulk,call_capability}`

`None` preserves the existing non-cancellable path. In particular,
`query_declarations_bulk(..., None)`, `declaration_*_bulk(..., None)`, and
`elaborate_bulk(..., None)` keep their single Lean-side bulk dispatch.

`Some(token)` enables cancellation checks. For singular methods, the token is
checked before each FFI dispatch point the Rust side controls. For bulk
methods, the Rust side switches to a per-item loop and checks the token between
items. That path is intentionally less batched: cancellation points require
returning to Rust.

## What Is Not Checked

The token does not interrupt work already running inside Lean. It cannot stop:

- one stuck `isDefEq` call that never returns to the host;
- a C-side kernel reduction that does not burn heartbeats;
- an `@[export]` symbol that runs its own long loop without checking
  heartbeats or returning to Rust;
- Lean runtime panics, aborts, `std::exit`, or foreign unwinds.

Use `LeanElabOptions` / `LeanMetaOptions` heartbeats for interpreter work that
does burn heartbeats. Use a caller-level timeout or a worker-process boundary
for work that may not return to a cooperative check point.

## Threading Model

`LeanCancellationToken` is `Clone + Send + Sync` because it contains only an
`Arc<AtomicBool>`. That does not make `LeanSession` cross-thread. Sessions,
Lean handles, capabilities, pools, and runtimes remain `!Send + !Sync` and stay
on the Lean worker thread.

The intended pattern is asymmetrical:

1. The worker thread owns the `LeanSession` and passes `Some(&token)` to the
   operation.
2. Another thread owns a clone of the token and calls `cancel()`.
3. The worker observes cancellation at the next host-controlled check point and
   returns `LeanError::Cancelled`.

The token is one-way. Create a fresh token for the next operation.

## Rejected Shapes

`*_with_cancellation` methods were rejected as a shallow duplicate API. They
double the public surface while providing the same operation with one extra
argument.

A session-wide cancellation wrapper was rejected because cancellation is an
operation-level decision. A token may apply to one user request, not to the
session's whole imported environment.

Pre-emptive cancellation was rejected because the host cannot safely unwind or
interrupt arbitrary Lean runtime work. Prompt 33 records the same boundary for
panic containment: when the host cannot prove Lean state and refcounts remain
valid, the recovery boundary is a worker process.
