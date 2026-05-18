# lean-rs-host

Opinionated Rust host stack for embedding Lean 4 as a theorem-prover capability.
Built on top of the [`lean-rs`](https://docs.rs/lean-rs) FFI primitive.

This crate provides the high-level `LeanHost` / `LeanCapabilities` / `LeanSession`
trio, the semantic-handle taxonomy (`LeanName`, `LeanLevel`, `LeanExpr`,
`LeanDeclaration`, `LeanEvidence`, `ProofSummary`), the bounded `MetaM` service
surface, and the `SessionPool` / `PooledSession` reuse helper.

If you only need to call typed `@[export]` Lean functions from Rust, depend on
`lean-rs` directly — it is the (β)-binding minimum and has no Lean-side shim
contract.

## Capability contract

The `LeanCapabilities::load_capabilities` entry point resolves 13 mandatory +
3 optional `lean_rs_host_*` symbols from the user's compiled Lake dylib. Today
those shims ship as test scaffolding in this repository's `fixtures/lean/`
Lake project; an external-consumer packaging story (Lake-require from git tag
vs. `build.rs`-bundled `liblean_rs_host.{so,dylib}`) is the prompt-30
deliverable per `RD-2026-05-18-001` in
`/Users/jcreinhold/Code/prompts/lean-rs/00-current-state.md`.

## License

Dual-licensed under MIT or Apache-2.0 at your option.
