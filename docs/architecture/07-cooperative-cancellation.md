# Cooperative Cancellation

`LeanCancellationToken` is a one-way, clonable flag. Pass `Some(&token)` to a host operation; another thread may call
`cancel()` on a clone. When the operation reaches a host-controlled check, it returns `LeanError::Cancelled` with code
`lean_rs.cancelled`. Create a fresh token for the next request.

```rust
let token = lean_rs_host::LeanCancellationToken::new();
let canceller = token.clone();
std::thread::spawn(move || canceller.cancel());

// On the thread that owns the session:
let declarations = session.query_declarations_bulk(&names, Some(&token), None)?;
```

The token contains only `Arc<AtomicBool>` and is `Send + Sync`. Sessions, capabilities, pools, runtime references, and
Lean handles remain thread-bound. Sharing the token does not permit sharing the session.

## Checkpoints and bulk work

Host query, elaboration, kernel, meta, and proof-workflow calls accept an optional cancellation token. They check it
before dispatch, with additional checks where the Rust implementation controls multiple dispatch points. Some methods
accept a progress sink after the token; see the [session API](https://github.com/jcreinhold/lean-rs/blob/main/crates/lean-rs-host/src/host/session.rs) for exact
signatures.

`query_declarations_bulk`, `declaration_type_bulk`, `declaration_kind_bulk`, `declaration_name_bulk`, and
`elaborate_bulk` use a Rust per-item loop when given a token. This creates checkpoints between items at the cost of
batching. On observed cancellation, collected results are discarded; no partial-output value is returned. Empty input
returns immediately.

With `None`, those methods retain their Lean-side bulk dispatch. Other operations, including fresh import, filtered
listing, kernel checking, and module processing, may enter one long Lean call after their pre-dispatch check. There is
no universal post-dispatch check: a token cancelled while that call runs need not turn its successful result into
`Cancelled`.

A progress sink may cancel a shared token, but that only changes the flag. A per-item path observes it at its next
check; a Lean-side progress callback does not itself add cancellation or stop semantics.

## Limits

The token cannot interrupt a running Lean call, a stuck `isDefEq`, C-side reduction, an export's own long loop, or a
fatal runtime failure. Heartbeat limits in `LeanElabOptions` and `LeanMetaOptions` bound work that burns heartbeats;
they do not guarantee wall-clock termination.

Use the worker process boundary for a request watchdog and recovery from work that does not return. Cancelling an async
join handle or timing out an in-process wait does not stop the underlying Lean work. Pre-emptive interruption would
leave object ownership and global runtime state unproven, the same reason [panic containment](06-panic-containment.md)
uses a process boundary.

The optional argument keeps cancellation at operation scope without duplicating every method as `*_with_cancellation` or
making a token part of a session's imported environment.
