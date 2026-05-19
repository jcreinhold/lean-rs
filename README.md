# lean-rs

[![CI](https://github.com/jcreinhold/lean-rs/actions/workflows/ci.yml/badge.svg)](https://github.com/jcreinhold/lean-rs/actions/workflows/ci.yml)
[![Sanitizer](https://github.com/jcreinhold/lean-rs/actions/workflows/sanitizer.yml/badge.svg)](https://github.com/jcreinhold/lean-rs/actions/workflows/sanitizer.yml)
[![Release](https://github.com/jcreinhold/lean-rs/actions/workflows/release.yml/badge.svg)](https://github.com/jcreinhold/lean-rs/actions/workflows/release.yml)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT_OR_Apache--2.0-blue.svg)](#license)

Rust bindings for hosting [Lean 4](https://lean-lang.org/) capabilities. Lean owns Lean
semantics—elaboration, kernel checking, proof objects, universes, `MetaM`, and dependent-type
meaning. This project owns hosting: linking, runtime initialization, ABI conversion, module
loading, error and panic boundaries, scheduling, diagnostics, batching, and packaging. Rust
does not reconstruct Lean semantic facts; that responsibility stays in Lean.

## Prerequisites

- [`elan`](https://github.com/leanprover/elan) and a Lean 4 toolchain in the [supported window](docs/version-matrix.md) (currently 4.26.0–4.29.1). `elan` ships `lean` and `lake` together.
- Rust stable (MSRV 1.91).
- macOS or Linux. Windows is not supported.

## Run the worked examples

Seven runnable examples live under [`crates/lean-rs-host/examples/`](crates/lean-rs-host/examples/). Build the in-tree fixture once, then run any of them:

```sh
(cd fixtures/lean && lake build)
cargo run -p lean-rs-host --example tour
```

`tour` composes the full host-stack flow (open → load capabilities → import session →
elaborate → kernel-check → bulk query → `Meta.whnf`) in one process; four focused examples
(`theorem_query`, `evaluate`, `proof_check`, `meta_query`) each isolate one verb. See
[`crates/lean-rs-host/examples/README.md`](crates/lean-rs-host/examples/README.md) for the
per-example walkthrough—what each one teaches, expected output, and common failures.

## Build your own consumer

The minimum L1 setup is five files. The example below calls a user-authored `@[export]` Lean
function from Rust without depending on `lean-rs-host`.

**`Cargo.toml`**—`lean-rs` for the API; `lean-toolchain` is a build-dep that emits link
directives and the runtime rpath:

```toml
[package]
name = "my_app"
version = "0.1.0"
edition = "2024"

[dependencies]
lean-rs = "0.1"  # pre-publish: also set `path = "../lean-rs/crates/lean-rs"`

[build-dependencies]
lean-toolchain = "0.1"
```

**`build.rs`**—one helper covers link-search, link-lib, and the runtime rpath into the
Lean toolchain's `lib/lean` directory; the other builds the Lake shared-library target
and records the dylib path for `main.rs`:

```rust
use std::path::Path;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    lean_toolchain::emit_lean_link_directives();
    let dylib = lean_toolchain::build_lake_target(Path::new("lean"), "MyCapability")?;
    println!("cargo:rustc-env=MY_CAPABILITY_DYLIB={}", dylib.display());
    Ok(())
}
```

**`lean/lakefile.lean`**—minimal Lake package emitting a shared library:

```lean
import Lake
open Lake DSL

package «my_app»

@[default_target]
lean_lib «MyCapability» where
  defaultFacets := #[LeanLib.sharedFacet]
```

**`lean/MyCapability.lean`**—one Rust-callable export:

```lean
@[export my_app_add]
def add (a b : UInt64) : UInt64 := a + b
```

**`src/main.rs`**—open the dylib, dispatch typed:

```rust
use lean_rs::{LeanLibrary, LeanResult, LeanRuntime};

fn main() -> LeanResult<()> {
    let runtime = LeanRuntime::init()?;
    let library = LeanLibrary::open(runtime, env!("MY_CAPABILITY_DYLIB"))?;
    let module = library.initialize_module("my_app", "MyCapability")?;

    let add = module.exported::<(u64, u64), u64>("my_app_add")?;
    println!("{}", add.call((40, 2))?);  // 42
    Ok(())
}
```

Build and run:

```sh
cargo run
```

`build_lake_target` hides Lake's shared-library facet, cache, and filename convention.
`initialize_module(package, lib_name)` still takes the unmangled Lake names.

For the L2 path (the `LeanHost` / `LeanCapabilities` / `LeanSession` stack with kernel
checking and `MetaM`), add `lean-rs-host = "0.1"` and follow
[`crates/lean-rs-host/README.md`](crates/lean-rs-host/README.md). That path also requires a
`require lean_rs_host_shims from ...` line in your `lakefile.lean`.

## The four published crates

`lean-rs-sys` is the raw Lean 4 C ABI binding: curated `extern "C"` declarations split by
semantic category, pure-Rust mirrors of `lean.h`'s `static inline` refcount helpers, the
`REQUIRED_SYMBOLS` allowlist, and the header digest. Public types (`lean_object`) are opaque;
layout is `pub(crate)`. Opt-in unsafe raw FFI; the safe layers in `lean-rs` are the recommended
path.

`lean-toolchain` provides Lean toolchain discovery, the typed `ToolchainFingerprint`, fixture
digest, layered link diagnostics, and `build.rs` helpers downstream embedders call from their
own build scripts (`emit_lean_link_directives` and `build_lake_target`, used above).

**`lean-rs` is the L1 FFI primitive.** Runtime initialization (token-bound `'lean` lifetime),
owned/borrowed object handles, typed ABI conversions, module loading, typed exported
functions, semantic handles (`LeanName`/`LeanLevel`/`LeanExpr`/`LeanDeclaration`), and a
structured error/diagnostic boundary. Ships zero Lean-side code; the Rust-to-Lean analog of
`ocaml-rs`.

**`lean-rs-host` is the L2 opinionated host stack.** The `LeanHost` / `LeanCapabilities` /
`LeanSession` trio, kernel-checked `LeanEvidence` and `ProofSummary`, the bounded `MetaM`
service registry, and `SessionPool` / `PooledSession`. Requires the 18 + 4 `lean_rs_host_*`
Lean shim contract in the capability dylib it loads.

The layering invariant is `lean-rs-sys` → `lean-toolchain` → `lean-rs` → `lean-rs-host`. Raw
`lean_object *` and raw `lean_*` symbols enter the workspace only via `lean-rs-sys` and are
not re-exported by `lean-toolchain` or `lean-rs`. The L1 (`lean-rs`) curated surface is the
typed FFI primitive plus the four core semantic handle types and the error boundary; the L2
(`lean-rs-host`) curated surface is the opinionated theorem-prover-host capability stack. See
[`docs/architecture/04-host-stack.md`](docs/architecture/04-host-stack.md) for the L2
classification table.

## Going deeper

Architecture and policy docs live under [`docs/architecture/`](docs/architecture/):

- [`00-charter.md`](docs/architecture/00-charter.md)—design boundary, hidden knowledge, smallest public interface, rejected alternatives.
- [`01-safety-model.md`](docs/architecture/01-safety-model.md)—unsafe boundary, reference-counting ownership, proof-object opacity, concurrency stance, and the workspace `unsafe-code = "deny"` policy.
- [`02-versioning-and-compatibility.md`](docs/architecture/02-versioning-and-compatibility.md)—supported Lean toolchain window, in-tree raw-FFI policy, crate semver, supported platforms.
- [`04-host-stack.md`](docs/architecture/04-host-stack.md)—the curated `lean-rs-host` surface and its semver boundary.
- [`04-concurrency.md`](docs/architecture/04-concurrency.md)—the `!Send + !Sync` contract and worker-thread attach discipline.
- [`05-raw-sys-design.md`](docs/architecture/05-raw-sys-design.md)—per-decision rationale behind `lean-rs-sys`.
- [`06-panic-containment.md`](docs/architecture/06-panic-containment.md)—why Lean internal panics are contained by process boundaries, not `LeanSession` poisoning.
- [`07-cooperative-cancellation.md`](docs/architecture/07-cooperative-cancellation.md)—the token-based cancellation contract and its non-preemptive limits.

Frozen public surfaces for each crate live under [`docs/api-review/`](docs/api-review/); later
changes diff against those baselines.

## Diagnostics

Every error-bearing public type projects to the stable
[`LeanDiagnosticCode`](crates/lean-rs/src/error/mod.rs) taxonomy via `.code()`, and both
crates emit structured `tracing` spans against the `lean_rs` target. See
[`docs/diagnostics.md`](docs/diagnostics.md) for the code catalogue, span catalogue,
recommended `RUST_LOG` scopes, and recipes for the in-process `DiagnosticCapture` test
affordance.

## Contributing

Workspace gates:

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo nextest run --workspace          # NOT `cargo test`—see docs/testing.md
cargo test --doc --workspace
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace
```

See [CONTRIBUTING.md](CONTRIBUTING.md) for contribution rules, including unsafe-code and
Lean-version-compatibility expectations.

## License

Dual-licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or <http://www.apache.org/licenses/LICENSE-2.0>)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or <http://opensource.org/licenses/MIT>)

at your option.
