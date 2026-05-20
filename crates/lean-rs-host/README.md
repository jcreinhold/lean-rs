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

## Quick start

The L2 setup adds a `lean-rs-host` dependency to `Cargo.toml`. The crate ships the matching
host and generic interop shim sources and builds them on demand, so your `lakefile.lean`
declares only your own capability library. Everything else mirrors the L1 setup in
[`lean-rs`'s README](https://docs.rs/lean-rs).

**`Cargo.toml`**:

```toml
[package]
name = "my_app"
version = "0.1.0"
edition = "2024"

[dependencies]
lean-rs = "0.1"
lean-rs-host = "0.1"

[build-dependencies]
lean-toolchain = "0.1"
```

**`build.rs`**—unchanged from the L1 setup; the checked helper reports Lean
toolchain failures as typed diagnostics:

```rust
fn main() -> Result<(), Box<dyn std::error::Error>> {
    lean_toolchain::emit_lean_link_directives_checked()?;
    Ok(())
}
```

**`lean/lakefile.lean`**—declares your capability target. Do not add a
`lean_rs_host_shims` require; `lean-rs-host` loads its bundled shims separately.

Long-running imports, bulk introspection, filtered listing, and kernel-check
calls can receive a borrowed `LeanProgressSink` for live in-thread progress
events. Passing `None` keeps the no-progress fast path.

```lean
import Lake
open Lake DSL

package «my_app»

@[default_target]
lean_lib «MyCapability» where
  defaultFacets := #[LeanLib.sharedFacet]
```

**`lean/MyCapability.lean`**—one Rust-callable export (the host stack also exposes the
shim contract's `@[export]` symbols, but you don't need to write any of those yourself):

```lean
@[export my_app_square]
def square (n : UInt64) : UInt64 := n * n
```

**`src/main.rs`**—open the Lake project as a `LeanHost`, load capabilities, drive a session:

```rust
use std::path::PathBuf;
use lean_rs::LeanResult;
use lean_rs::LeanRuntime;
use lean_rs_host::{LeanElabOptions, LeanHost};

fn main() -> LeanResult<()> {
    let lake_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("lean");

    let runtime = LeanRuntime::init()?;
    let host = LeanHost::from_lake_project(runtime, &lake_root)?;
    let caps = host.load_capabilities("my_app", "MyCapability")?;
    let mut session = caps.session(&["MyCapability"], None, None)?;

    // Elaborate a Lean term in the session's environment.
    let opts = LeanElabOptions::new();
    let elaborated = session.elaborate("(1 + 2 : Nat)", None, &opts, None)?;
    println!("elaborate ok: {}", elaborated.is_ok());

    // Call your own typed @[export] through the instrumented session dispatch.
    let n = session.call_capability::<(u64,), u64>("my_app_square", (7u64,), None)?;
    println!("square(7) = {n}");  // 49

    Ok(())
}
```

Build and run:

```sh
(cd lean && lake build)
cargo run
```

Lake produces your `libmy__app_MyCapability.{dylib,so}`. `load_capabilities` also builds and
opens the crate-bundled `LeanRsInterop` and `LeanRsHostShims` dylibs, sharing one Lean
runtime; per-module `initialize_*` functions are idempotent.

Pre-publish, pin against the workspace by adding `path =` lines alongside the `version`
constraints in `Cargo.toml` (same as the L1 setup).

## Capability contract

The full per-symbol contract—each Lean signature, the Rust call site it maps to, and the
typed `LeanSession::*` method on top—lives at
[`docs/lean-rs-host-capability-contract.md`](https://github.com/jcreinhold/lean-rs/blob/main/docs/lean-rs-host-capability-contract.md).
Your `lean_lib` does **not** need to `import LeanRsHostShims`; the Rust side builds and
loads the bundled shim dylibs and then dispatches through their fixed symbols.

## Caveats

**Nullary unboxed-scalar globals trip the function-path dispatch.** A nullary `@[export]`
returning an unboxed scalar (e.g., `def decideTrue : Bool := decide (1 + 1 = 2)`) is
compiled by Lake as a persistent global, not a function symbol. `LeanModule::exported`'s
function-path dispatch then reads the global's stored scalar-tagged value as if it were a
function pointer, and `.call(...)` panics with `misaligned pointer dereference`. Workaround:
add a `Unit` argument so Lake emits a function symbol:

```lean
def decideTrue (_ : Unit) : Bool := decide (1 + 1 = 2)
```

The Rust call site then becomes `module.exported::<((),), bool>(...).call(())`.

**Pre-publish `lean-rs = "0.1"` and `lean-rs-host = "0.1"` need path pins.** Until the crates
land on crates.io, the consumer's `Cargo.toml` pins both `version = "0.1"` and
`path = "..."`. Cargo enforces the version constraint at build time; once published, drop
the `path =` attribute.

## Worked examples

Eight runnable examples under
[`crates/lean-rs-host/examples/`](https://github.com/jcreinhold/lean-rs/tree/main/crates/lean-rs-host/examples)
drive `lean_rs_host::*` end to end against the in-tree fixture:

- `theorem_query`—open a session, contrast a definition's `kind` with a theorem's.
- `evaluate`—call a typed `@[export]` through `LeanSession::call_capability`.
- `proof_check`—kernel-check a theorem, re-validate the evidence, render the summary.
- `meta_query`—run a bounded `MetaM` service and branch on every status.
- `progress`—attach a `LeanProgressSink` and trigger cooperative cancellation.
- `tour`—the four flows composed end to end in one process.
- `lake_build_helper`—build a Lake shared-library target through `lean-toolchain`.
- `long_session_memory`—capture RSS checkpoints for a long-lived session workload.

See the
[examples README](https://github.com/jcreinhold/lean-rs/blob/main/crates/lean-rs-host/examples/README.md)
for the per-example walkthrough, expected output, and common failures.

## License

Dual-licensed under MIT or Apache-2.0 at your option.
