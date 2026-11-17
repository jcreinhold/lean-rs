# Runtime Model Proof Package

`formal/RuntimeModel/` contains the abstract Lean model for the
[worker runtime contract](30-worker-runtime-semantics.md). It is separate from ABI fixtures, shipped shims, and consumer
templates, so abstract proofs neither become runtime payloads nor depend on native loading.

## Current package

The Lake package is `lean_rs_runtime_model`, with default library `RuntimeModel`. Its `lean-toolchain` matches the
repository pin. It has no Mathlib or other package dependency and uses only Lean's implicit `Init` plus its own modules:

| Module | Contents |
| --- | --- |
| `RuntimeModel.Basic` | Generation/request/lease ids, outcomes, events, generation and completion predicates |
| `RuntimeModel.Worker` | Worker phases, functional terminal ledger, abstract steps and finite traces |
| `RuntimeModel.Pool` | Lease statuses, reuse predicate, and lease-state updates |
| `RuntimeModel.lean` | Public module imports |

The source contains five theorem declarations:

- `stale_generation_events_cannot_complete`: a completing event's generation agrees with the request generation;
- `terminal_outcome_unique`: two outcomes stored for the same state/generation/request are equal;
- `consumed_lease_not_reusable`;
- `released_lease_not_reusable`;
- `consumed_or_released_lease_not_reusable`.

These are small abstract facts. In particular, ledger lookup uniqueness does not by itself prove that every Rust
execution records at most one terminal event. The package does not yet prove implementation-trace refinement,
shutdown liveness, restart fairness, or the full native process/guardian contract. Rust conformance tests provide
separate implementation evidence; their private trace vocabulary is not a proof of refinement.

## Verification

The formal package is checked separately from routine Rust tests. From the repository root:

```sh
cd formal/RuntimeModel
lake build
lake env lean --stdin <<'LEAN'
import RuntimeModel
#print axioms RuntimeModel.stale_generation_events_cannot_complete
#print axioms RuntimeModel.terminal_outcome_unique
#print axioms RuntimeModel.consumed_lease_not_reusable
#print axioms RuntimeModel.released_lease_not_reusable
#print axioms RuntimeModel.consumed_or_released_lease_not_reusable
LEAN
```

Do not treat a theorem as checked release evidence without a successful build and axiom inspection. Project axioms,
`sorry`, and `admit` are excluded from checked evidence. Record the toolchain and revision with the result.

A runtime-model release also runs the [workspace gates](https://github.com/jcreinhold/lean-rs/blob/main/CONTRIBUTING.md), `mdbook build docs`, and the named
[worker probes](../performance.md) for startup, shutdown, restart, pool admission, dispatch, streaming, and backpressure.
Performance reports record command, platform, row counts, throughput, parent/child RSS availability, and fallback versus
real mathlib module-list workload.

## Maintenance

A change to supervisor, session, pool, child, or protocol behavior that changes a model clause must update both the
prose contract and the corresponding abstract model. Keep the formal package import-light and independent of
`libleanshared`, fixture packages, host shims, and runtime integration tests. New claims need explicit assumptions and
small meaningful lemmas; profile expensive elaboration before increasing heartbeat budgets.
