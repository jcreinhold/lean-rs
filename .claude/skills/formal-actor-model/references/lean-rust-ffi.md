# Lean/Rust FFI Actor Boundaries

An FFI actor boundary is unsafe until the ownership and thread-safety story is explicit. Keep the ABI small and move
semantic work into Lean or Rust modules that can be tested in their native language.

## Boundary Shape

Prefer opaque handles:

- Rust owns the actor runtime and exports a pointer-like opaque handle.
- Lean receives a handle token but cannot inspect Rust state.
- Every handle has explicit `create`, `send/call`, `status`, and `destroy` operations.
- Destroy is idempotent when possible; if not, document the exact precondition.

Avoid passing Rust structs or Lean objects by layout. Use stable serialized messages or handles.

## Lean ABI Ownership

For `lean_object *` values:

- Owned parameters transfer a virtual reference-count token to the callee.
- Borrowed parameters (`@&` on the Lean side) do not transfer ownership.
- Return values are owned.
- The side that owns a value must decrement/free it exactly once.

When in doubt, copy/serialize at the boundary and keep ownership local.

## Initialization

Before calling Lean from Rust-created code:

- Initialize the Lean runtime and imported modules exactly once.
- Run required module initializers before using exported declarations.
- Initialize any non-Lean-created thread before it touches Lean objects.
- Finalize threads that were initialized for Lean when they exit.

Never call Lean declarations from a foreign thread and assume the runtime is already prepared.

## Callbacks

Callbacks are the highest-risk part of the design.

Rules:

- Register callbacks with explicit user data/handle.
- Unregister callbacks before destroying the target object.
- Ensure no callback can run after destruction.
- If a callback arrives on a foreign thread, enqueue an actor message; do not mutate Lean or Rust actor state directly.
- Do not call back into Lean while holding a Rust mutex unless the lock order is documented and tested.

## Message Representation

Choose one:

- Serialized JSON/CBOR/bytes: stable and robust, slower, easiest across versions.
- Opaque IDs plus side tables: efficient but requires lifecycle proofs.
- Lean inductive values crossing the ABI: only when both sides tightly control Lean object ownership and initialization.

Do not mix all three casually. One boundary should have one representation policy.

## Safety Contract Template

Each unsafe FFI module should state:

- Who owns every handle and object.
- Which functions may be called from which threads.
- Whether functions are reentrant.
- What happens after `destroy`.
- What happens on panic, exception, or worker death.
- Whether messages may be lost, duplicated, or reordered.
- How cancellation and shutdown are observed.

## Tests

- Create/destroy repeatedly.
- Send after destroy returns a structured error.
- Callback after unregister is impossible or ignored safely.
- Foreign-thread callback enqueues into the actor and does not mutate state directly.
- Runtime restart invalidates or refreshes stale handles according to the documented policy.
