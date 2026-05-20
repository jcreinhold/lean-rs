# Callback Payloads

The original two-counter callback payload is a progress tick. It was enough to
prove the callback handle, trampoline, stale-handle, panic, and reentrancy
contracts, but it is not a general Lean-to-Rust callback payload model.

The next callback boundary keeps the same deep-module rule as the existing L1
registry: `lean-rs` owns callback handle lifetime, trampoline safety, payload
decoding, panic containment, stale handles, and wrong-payload handling. Callers
should not learn raw Lean object lifetimes, refcount rules, or callback ABI
details to receive a payload.

## Designs Considered

**Single enum payload.** One callback handle could accept an enum such as
`Tick | String | Bytes`. This keeps one public handle type, but it makes the
handle less precise. Every callback site would carry variants it does not
accept, and a Lean-side wrong payload would become normal runtime branching
instead of a boundary error. Progress reporting would also remain visible in
the low-level payload vocabulary.

**Fully user-defined payloads.** Downstream crates could implement a public
`LeanCallbackPayload` trait. That would look flexible, but it would push the
wrong complexity upward. A sound implementation would have to know how Lean
objects cross the ABI, when strings are retained or copied, how wrong payloads
are detected, and which panic path is allowed through the trampoline. Those are
registry concerns, not consumer concerns.

**Sealed typed payload family.** This is the chosen design. `lean-rs` owns the
supported payload shapes, and a callback handle registers one shape at a time:

```rust
LeanCallbackHandle<P: LeanCallbackPayload>
```

`LeanCallbackPayload` is sealed. Downstream crates can use supported payloads,
but they cannot add ABI shapes by implementing the trait themselves. This keeps
the public API general enough for current use cases without turning callback
payload decoding into a user extension point.

## Initial Payloads

The first two payloads are:

```rust
pub struct LeanProgressTick {
    pub current: u64,
    pub total: u64,
}

pub struct LeanStringEvent {
    pub value: String,
}
```

`LeanProgressTick` carries the existing counter payload. `lean-rs-host`
continues to own progress policy: phase names, elapsed time, cancellation
checkpoints, and which `LeanSession` methods emit events. The host layer may
map a `LeanProgressTick` into `LeanProgressEvent`, but progress is not the L1
callback abstraction. It is an observability/control signal, not a data row.

`LeanStringEvent` is the next useful L1 payload. It supports downstream
same-process line-oriented protocols: Lean can emit one encoded line at a time,
Rust receives owned strings, and neither side has to tunnel through subprocess
stdout. Worker-style tools should not expose this handle to parent callers;
`lean-rs-worker` turns child-local callbacks into typed worker rows when a
process boundary is needed. The runnable L1 proof is
[`../recipes/string-callback-streaming.md`](../recipes/string-callback-streaming.md).

Byte arrays and arbitrary Lean-object callbacks are deferred. They should land
only when a measured same-process L1 consumer needs them, because each payload
adds ABI, ownership, and diagnostics surface area. Worker JSON/raw-JSON rows do
not require new callback payloads.

## Error Boundary

The typed registry must distinguish these cases:

- the callback ran and wants Lean to continue;
- the callback ran and asks Lean to stop;
- Lean called a stale handle;
- Rust contained a callback panic;
- Lean called a handle with the wrong payload shape.

The public status vocabulary should express those outcomes without exposing raw
pointers or Lean object references. Wrong-payload handling belongs in `lean-rs`
because the registry knows the payload type associated with each handle.

## Relationship To Existing Docs

[`10-callback-registry.md`](10-callback-registry.md) documents the implemented
typed registry. The counter event is `LeanProgressTick`, and string callbacks
use `LeanCallbackHandle<LeanStringEvent>`.

[`13-structured-progress.md`](13-structured-progress.md) remains host policy.
Its progress sink is implemented over `LeanProgressTick`, not a general
callback event.

[`14-interop-release-contract.md`](14-interop-release-contract.md) remains
useful for the interop stack as a whole: explicit exports, build helpers,
callback handles, bundled shims, and examples. The callback payload part of
that release contract is the narrow piece being revised.
