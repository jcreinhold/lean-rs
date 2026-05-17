# Test Taxonomy

Use the lightest surface that can catch the real bug.

## Unit tests

Use for a local contract with a clear oracle.

- Good for: helper functions, ordering rules, DTO conversions, conflict checks
- Avoid when: the bug only appears across module boundaries or in public behavior

## Integration tests

Use for public outcomes or cross-module behavior.

- Good for: CLI behavior, registry views, crate-boundary contracts
- Avoid when: the invariant is purely local and the integration surface adds noise

## Property tests

Use when the contract is best stated as a law over many inputs.

- Good for: roundtrips, lattice/order laws, binding laws, preservation properties
- Avoid when: you do not have a meaningful generator or oracle

## Compile-fail / UI tests

Use when the contract is "this must be rejected" and the shape of the rejection matters.

- Good for: proc macros, parser/typechecker rejection, trait or visibility errors
- Avoid when: a stable error variant or local negative unit test would do

## Snapshot tests

Use only for stable user-facing structure.

- Good for: formatted output with deliberate structure, structured diagnostics
- Avoid when: wording churn is expected or the snapshot would mostly capture noise

## Benches and profiling

Use when the risk is time, allocation, or scaling.

- Good for: throughput regressions, hot-path scaling, allocation blowups
- Avoid when: the real risk is semantic correctness

If the best answer is a bench, switch to `optimizing-rust-performance`.
