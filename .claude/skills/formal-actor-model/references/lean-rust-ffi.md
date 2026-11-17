# Lean/Rust FFI Actor Boundaries

Keep the ABI small and document ownership and threads before making it safe. In lean-rs, follow the established
[safety model](../../../../docs/architecture/01-safety-model.md), conversion APIs, and callback registry rather than
adding a parallel raw boundary.

## Handles and ownership

Use opaque handles with explicit lifecycle operations. The owning runtime keeps its state private; callers receive
capabilities to send, call, query status, or destroy. Specify whether destroy is idempotent and what happens to later
calls and stale generations.

At a raw Lean ABI crossing:

- Owned object parameters transfer a reference-count token to the callee.
- Borrowed parameters (`@&` in Lean) retain that token with the caller.
- Object returns carry an owned token.
- Each token has one owner responsible for releasing it.

Keep this bookkeeping inside the existing safe layer. Do not duplicate Rust struct layouts or Lean object-layout
knowledge in actor code. Serialize or copy payloads when that keeps ownership local.

## Initialization and threads

The runtime wrapper must initialize Lean, attach foreign threads as required, and initialize modules before using
exports. Finalize attached threads according to the wrapper's lifetime contract. A prepared thread still does not make
an existing Lean object transferable from another thread.

## Callbacks

- Register callbacks through an owned registration handle and unregister before destroying the target.
- Prevent callbacks after destruction, including races with unregister and restart.
- For a foreign-thread callback, enqueue owned Rust data permitted by the thread contract. Do not enqueue Lean-derived
  handles or directly mutate another actor's state.
- Do not call back into Lean while holding a Rust mutex unless reentrancy and lock order are documented and tested.
- Catch Rust panics before the C boundary and define how callback failure reaches the caller.

## Message representation

Choose a representation policy for each crossing:

- Serialized bytes keep runtimes independent; measure their cost for the workload.
- Opaque IDs need side-table lifetime and generation checks.
- Lean inductive objects need both sides to enforce the same ownership, initialization, and thread contract.

Document any translation between representations instead of mixing them implicitly.

## Safety contract and checks

For each unsafe boundary, state the owner of every handle, allowed threads, reentrancy, post-destroy behavior, panic and
exception handling, worker-death behavior, delivery/ordering guarantees, and cancellation/shutdown semantics.

Check repeated create/destroy, calls after destroy, unregister races, foreign callbacks, and generation changes after
restart. The tests should fail under a plausible ownership or lifecycle violation. A runtime model does not replace
memory-safety review of the FFI implementation.
