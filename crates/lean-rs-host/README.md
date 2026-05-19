# lean-rs-host

Opinionated Rust host stack for embedding Lean 4 as a theorem-prover capability. Provides the
`LeanHost` / `LeanCapabilities` / `LeanSession` trio, the kernel-check evidence types
(`LeanEvidence`, `LeanKernelOutcome`, `ProofSummary`), the typed elaboration diagnostics
(`LeanElabOptions`, `LeanElabFailure`, `LeanDiagnostic`, `LeanSeverity`, `LeanPosition`), the
bounded `MetaM` service surface at `lean_rs_host::meta::*`, and the `SessionPool` /
`PooledSession` reuse helper.

Built on top of [`lean-rs`](https://docs.rs/lean-rs), the L1 typed-FFI primitive. The opaque
semantic handles `LeanName`, `LeanLevel`, `LeanExpr`, and `LeanDeclaration` live on `lean-rs`;
this crate consumes them through `use lean_rs::{...}`. If you only need to call typed
`@[export]` Lean functions from Rust, depend on `lean-rs` directly—it is the typed-FFI
minimum and has no Lean-side shim contract.

Supports the same Lean toolchain window as
[`lean-rs-sys`](https://docs.rs/lean-rs-sys)—currently **Lean 4.26.0 through 4.29.1**; see
[`docs/version-matrix.md`](https://github.com/jcreinhold/lean-rs/blob/main/docs/version-matrix.md).
The capability loader transparently handles the Lake naming-convention change between Lean
4.26 and 4.27 (dylib filename and module-initializer symbol shape), so consumer
`lakefile.lean`s do not need version-conditional logic.

## Capability contract

`LeanCapabilities::load_capabilities` opens two dylibs and resolves a fixed contract of 13
mandatory + 3 optional `@[export] lean_rs_host_*` symbols. The contract is satisfied by a
separately-distributed Lake package,
[**`lean-rs-host-shims`**](https://github.com/jcreinhold/lean-rs/tree/main/lake/lean-rs-host-shims),
that ships from the same repository as this crate. Add to your `lakefile.lean`:

```lean
import Lake
open System Lake DSL

package «my_app»

require «lean_rs_host_shims» from "../lean-rs/lake/lean-rs-host-shims"

@[default_target]
lean_lib «MyCapability» where
  defaultFacets := #[LeanLib.sharedFacet]
```

Lake builds two dylibs (yours plus the shim package's). At runtime,
`LeanCapabilities::load_capabilities` opens the shim dylib first with `RTLD_GLOBAL` (so your
dylib's transitive references resolve) and then opens your dylib. Both dylibs share one Lean
runtime; per-module `initialize_*` functions are idempotent.

The full per-symbol contract—each Lean signature, the Rust call site it maps to, and the
typed `LeanSession::*` method on top—lives at
[`docs/lean-rs-host-capability-contract.md`](https://github.com/jcreinhold/lean-rs/blob/main/docs/lean-rs-host-capability-contract.md).
Your `lean_lib` does **not** need to `import LeanRsHostShims`; Lake's two-package build
handles the dylib-level wiring and the Rust side does the runtime dispatch.

A standalone external-consumer proof—`lean-rs-host-downstream`—exercises
`LeanHost` → `LeanCapabilities` → `LeanSession` end to end, including `query_declaration`,
`kernel_check`, `summarize_evidence`, and `LeanSession::call_capability` for a user-authored
`@[export]` symbol.

## License

Dual-licensed under MIT or Apache-2.0 at your option.
