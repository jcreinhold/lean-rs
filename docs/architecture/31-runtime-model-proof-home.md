# Runtime Model Proof Home

This document decides where checked Lean proofs for
[`30-worker-runtime-semantics.md`](30-worker-runtime-semantics.md) belong. The decision is architectural: this prompt
does not add Lean files or a Lake package.

## Decision

`lean-rs` should carry a durable, lightweight proof package for the worker runtime model under
`formal/RuntimeModel/`.

The package is not a fixture, not a worker child, and not a runtime integration test. It is a small Lean development for
the abstract transition systems and invariants named in the canonical runtime model. Prompt 36 created this package
skeleton.

## Existing Lean Roots

The existing Lake roots are not the proof home:

- `fixtures/lean/` is the `LeanRsFixture` ABI-boundary fixture package. It exports stable symbols for Rust tests,
  examples, and benchmarks.
- `fixtures/interop-shims/` is a consumer fixture for reusable interop shims.
- `crates/lean-rs/shims/lean-rs-interop-shims/` and `crates/lean-rs-host/shims/*` are shipped shim packages used by the
  runtime and host crates.
- `templates/shipped-lean-crate/lean/` is a consumer template.

Durable proofs should not live in any of those roots. Mixing proofs into fixtures would make test payloads look like
specification authority, and mixing them into shims would couple abstract proofs to runtime-loading artifacts.

## Package Shape

Prompt 36 should create this minimal shape:

```text
formal/RuntimeModel/
  lean-toolchain
  lakefile.lean
  RuntimeModel/
    Basic.lean
    Worker.lean
    Pool.lean
```

The package should use the repository root `lean-toolchain` version. Its Lake package name should be
`lean_rs_runtime_model`, and its default library should be `RuntimeModel`.

The first package should import only `Init` unless a specific theorem needs a narrow Lean standard-library module. It
must not depend on `libleanshared`, the worker child binary, fixture packages, host shims, or import-heavy Lean runtime
behavior. It should model requests, responses, rows, generations, leases, worker states, pool states, events, and trace
predicates as ordinary inductive types and structures.

The verification command for Prompt 36 is:

```sh
cd /Users/jcreinhold/Code/lean-rs/formal/RuntimeModel
lake build
```

Routine Rust verification does not run this command. A later release gate may opt in explicitly after the package is
stable and measured.

## Proof Scope

The initial proof package should target the stable model labels from
[`30-worker-runtime-semantics.md`](30-worker-runtime-semantics.md), not implementation details. Suggested theorem names:

- `terminal_outcome_unique`
- `generation_separation`
- `affine_lease_consumed_once`
- `serial_request_step_unique`
- `restart_exhaustion_stops_admission`
- `stale_generation_rows_rejected`
- `shutdown_eventually_reaches_terminal_state_under_assumptions`
- `implementation_trace_refines_model_trace`

The first package does not need to prove every theorem. It should define the vocabulary and enough statements for
later Rust refactors to cite. Every proof theorem must avoid `sorry`, `admit`, and project axioms before it is treated
as checked evidence.

## Ownership

The proof package belongs to the worker runtime architecture, not to downstream hosts. Changes to
`supervisor.rs`, `session.rs`, `pool.rs`, `lean-rs-worker-child`, or `lean-rs-worker-protocol` that change a model
clause must update both the prose model and the proof package once the package exists.

Proof files should follow mathlib-style discipline: narrow imports, meaningful theorem names, explicit statements,
small helper lemmas with real mathematical content, and build-clean files. Performance-sensitive proofs should be
profiled before increasing heartbeat budgets.
