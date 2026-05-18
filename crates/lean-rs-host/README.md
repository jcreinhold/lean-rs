# lean-rs-host

Opinionated Rust host stack for embedding Lean 4 as a theorem-prover capability.
Built on top of the [`lean-rs`](https://docs.rs/lean-rs) FFI primitive.

This crate provides the high-level `LeanHost` / `LeanCapabilities` / `LeanSession`
trio, the kernel-check evidence types (`LeanEvidence`, `LeanKernelOutcome`,
`ProofSummary`), the typed elaboration diagnostics (`LeanElabOptions`,
`LeanElabFailure`, `LeanDiagnostic`, `LeanSeverity`, `LeanPosition`), the
bounded `MetaM` service surface at `lean_rs_host::meta::*`, and the
`SessionPool` / `PooledSession` reuse helper.

The opaque semantic handles `LeanName`, `LeanLevel`, `LeanExpr`, and
`LeanDeclaration` live on the L1 [`lean-rs`](https://docs.rs/lean-rs) crate
along with the runtime, library, module, typed-export, and error model;
`lean-rs-host` consumes them through `use lean_rs::{...}`.

If you only need to call typed `@[export]` Lean functions from Rust, depend on
`lean-rs` directly — it is the (β)-binding minimum and has no Lean-side shim
contract.

## Capability contract

The `LeanCapabilities::load_capabilities` entry point resolves 13 mandatory +
3 optional `lean_rs_host_*` symbols from the user's compiled Lake dylib.
Today those shims ship as test scaffolding inside the in-tree workspace
fixture at `fixtures/lean/LeanRsFixture/` (visible in the [project
repository](https://github.com/jcreinhold/lean-rs)). An external-consumer
packaging story for the shims (Lake-require from git tag vs.
`build.rs`-bundled `liblean_rs_host.{so,dylib}`) is the prompt-30
deliverable per `RD-2026-05-18-001`; until that lands, building a capability
against this crate requires copying or re-implementing the shim contract.

## License

Dual-licensed under MIT or Apache-2.0 at your option.
