# lean-rs-host

Opinionated Rust host stack for embedding Lean 4 as a theorem-prover capability.
Built on top of the [`lean-rs`](https://docs.rs/lean-rs) FFI primitive.

Supports the same Lean toolchain window as
[`lean-rs-sys`](https://docs.rs/lean-rs-sys) — currently **Lean 4.26.0
through 4.29.1**; see
[`docs/version-matrix.md`](https://github.com/jcreinhold/lean-rs/blob/main/docs/version-matrix.md).
The capability loader transparently handles the Lake naming-convention
change between Lean 4.26 and 4.27 (dylib filename and module-initializer
symbol shape), so consumer `lakefile.lean`s do not need version-conditional
logic.

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

`LeanCapabilities::load_capabilities` opens two dylibs and resolves a
fixed contract of 13 mandatory + 3 optional `@[export] lean_rs_host_*`
symbols. The contract is satisfied by a separately-distributed Lake
package, **`lean-rs-host-shims`**, that ships from the same repository
as this crate. Your `lakefile.lean` adds:

```lean
import Lake
open System Lake DSL

package «my_app»

-- Pre-publish: path-require against a sibling checkout.
require «lean_rs_host_shims» from "../lean-rs/lake/lean-rs-host-shims"
-- Post-publish (planned, prompt 30): git-require by tag.
-- require «lean_rs_host_shims» from git "https://github.com/jcreinhold/lean-rs" @ "v0.1.0" / "lake/lean-rs-host-shims"

@[default_target]
lean_lib «MyCapability» where
  defaultFacets := #[LeanLib.sharedFacet]
```

Lake builds two dylibs (yours + the shim package's). At runtime
`LeanCapabilities::load_capabilities` opens the shim dylib first
with `RTLD_GLOBAL` (so your dylib's transitive references resolve)
and then opens your dylib. Both dylibs share one Lean runtime;
per-module `initialize_*` functions are idempotent.

The full per-symbol contract is documented at
[`docs/lean-rs-host-capability-contract.md`](https://github.com/jcreinhold/lean-rs/blob/main/docs/lean-rs-host-capability-contract.md)
— each Lean signature, the Rust call site it maps to, and the typed
`LeanSession::*` method on top. Your `lean_lib` does **not** need to
`import LeanRsHostShims`; Lake's two-package build handles the
dylib-level wiring and the Rust side does the runtime dispatch.

### Worked external-consumer proof

A standalone repo demonstrating the full setup against an
out-of-workspace Lake project:
[`/Users/jcreinhold/Code/lean-rs-host-downstream/`](https://github.com/jcreinhold/lean-rs-host-downstream)
(if/when published). It mirrors `lean-rs-downstream` (the L1 proof)
but exercises `LeanHost` → `LeanCapabilities` → `LeanSession` end to
end, including `query_declaration`, `kernel_check`,
`summarize_evidence`, and `LeanSession::call_capability` for a
user-authored `@[export]` symbol.

## License

Dual-licensed under MIT or Apache-2.0 at your option.
