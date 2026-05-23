# lean-rs

[![CI](https://github.com/jcreinhold/lean-rs/actions/workflows/ci.yml/badge.svg)](https://github.com/jcreinhold/lean-rs/actions/workflows/ci.yml)
[![Sanitizer](https://github.com/jcreinhold/lean-rs/actions/workflows/sanitizer.yml/badge.svg)](https://github.com/jcreinhold/lean-rs/actions/workflows/sanitizer.yml)
[![Release](https://github.com/jcreinhold/lean-rs/actions/workflows/release.yml/badge.svg)](https://github.com/jcreinhold/lean-rs/actions/workflows/release.yml)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT_OR_Apache--2.0-blue.svg)](#license)

Rust bindings for hosting [Lean 4](https://lean-lang.org/) capabilities. Lean owns Lean semantics (elaboration, kernel
checking, proof objects, universes, `MetaM`, dependent-type meaning). This project owns hosting: linking, runtime
initialization, ABI conversion, module loading, error and panic boundaries, scheduling, diagnostics, batching, and
packaging. Rust does not reconstruct Lean semantic facts.

## Prerequisites

- [`elan`](https://github.com/leanprover/elan) and a Lean 4 toolchain in the [supported window](docs/version-matrix.md)
  (currently 4.26.0–4.29.1). `elan` ships `lean` and `lake` together.
- Rust stable (MSRV 1.91).
- macOS or Linux. Windows is not supported.

## Start Here

If you want to ship a Rust crate that owns Lean source, start with the checked template:

```sh
cargo run --manifest-path templates/shipped-lean-crate/Cargo.toml
cargo build --manifest-path templates/shipped-lean-crate/Cargo.toml --bin shipped-lean-crate-worker
cargo run --manifest-path templates/shipped-lean-crate/Cargo.toml --example worker
```

That path builds the Lean shared library in `build.rs`, opens it from Rust, and starts the same capability behind an
app-owned worker child. The full recipe is
[`docs/recipes/ship-crate-with-lean.md`](docs/recipes/ship-crate-with-lean.md).

The in-tree host tour is a workspace orientation example, not the canonical shipped-crate layout. Build the fixture
once, then run the tour:

```sh
cd fixtures/lean && lake build && cd -
cargo run -p lean-rs-host --example tour
```

`tour` composes the full host-stack flow in one process: open the runtime, load capabilities, open an import session,
elaborate, kernel-check, run a bulk query, and call `Meta.whnf`.

Browse the eight examples and walkthroughs at
[`crates/lean-rs-host/examples/README.md`](crates/lean-rs-host/examples/README.md). That's the host-stack tour path for
sessions, kernel checks, and `MetaM`.

If something goes wrong, re-run with `RUST_LOG=lean_rs=debug` for structured spans. See
[`docs/diagnostics.md`](docs/diagnostics.md) for the code catalogue and the in-process capture API.

## The five published crates

Choose by job:

| Job | Start with |
| --- | --- |
| Ship a Rust crate with Lean source | `lean-toolchain` (build-time) plus `lean-rs` or `lean-rs-worker` (runtime) |
| Call a Lean `@[export]` from Rust in the same process | `lean-rs` |
| Use imports, elaboration, kernel checks, declaration queries, or `MetaM` | `lean-rs-host` |
| Run production worker-style tools (process isolation, live rows, timeouts, cycling) | `lean-rs-worker` |
| Bind raw Lean C symbols directly (advanced, `unsafe`) | `lean-rs-sys` |

Layering: `lean-rs-sys` → `lean-toolchain` → `lean-rs` → `lean-rs-host`; `lean-rs-worker` wraps the host stack in a
child-process boundary. Raw `lean_*` symbols enter the workspace only through `lean-rs-sys`; the safe layers never
re-export them. Lower layers are escape hatches, not steps every downstream caller should hand-compose. See
[`docs/architecture/03-host-stack.md`](docs/architecture/03-host-stack.md) for the host-stack classification table.

## Call a Lean export from Rust

The minimum same-process setup is five pieces: a `Cargo.toml`, a `build.rs`, a Lake `lakefile.lean`, a Lean module, and
a Rust `main.rs`. The example calls a user-authored `@[export]` Lean function from Rust without depending on
`lean-rs-host`.

All five crates are published on crates.io at the same workspace version (currently 0.1.2). The `Cargo.toml` snippets in
this repo use `"0.1"` so they pick up the latest 0.1.x.

**`Cargo.toml`**: `lean-rs` for the API; `lean-toolchain` is a build-dep that emits link directives and the runtime
rpath:

```toml
[package]
name = "my_app"
version = "0.1.0"
edition = "2024"

[dependencies]
lean-rs = "0.1"

[build-dependencies]
lean-toolchain = "0.1"
```

**`build.rs`**: the high-level helper covers link-search, link-lib, the runtime rpath, Lake build, Cargo rerun
directives, and the compile-time dylib path:

```rust
fn main() -> Result<(), Box<dyn std::error::Error>> {
    lean_toolchain::CargoLeanCapability::new("lean", "MyCapability")
        .package("my_app")
        .module("MyCapability")
        .build()?;
    Ok(())
}
```

**`lean/lakefile.lean`**: minimal Lake package emitting a shared library. Lake uses guillemets (`« »`) as its idiomatic
quoting for package and library names; plain ASCII names also work.

```lean
import Lake
open Lake DSL

package «my_app»

@[default_target]
lean_lib «MyCapability» where
  defaultFacets := #[LeanLib.sharedFacet]
```

**`lean/MyCapability.lean`**: one Rust-callable export:

```lean
@[export my_app_add]
def add (a b : UInt64) : UInt64 := a + b
```

**`src/main.rs`**: open the build-script capability, dispatch typed:

```rust
use lean_rs::{LeanBuiltCapability, LeanCapability, LeanResult, LeanRuntime};

fn main() -> LeanResult<()> {
    let runtime = LeanRuntime::init()?;
    let capability = LeanCapability::from_build_manifest(
        runtime,
        LeanBuiltCapability::manifest_path(env!("LEAN_RS_CAPABILITY_MY_CAPABILITY_MANIFEST")),
    )?;
    let module = capability.module()?;

    let add = module.exported::<(u64, u64), u64>("my_app_add")?;
    println!("{}", add.call(40, 2)?);  // 42
    Ok(())
}
```

Build and run:

```sh
cargo run
```

`CargoLeanCapability` hides Lean link directives, Lake's shared-library facet, Cargo rerun triggers, cache, filename
convention, and the artifact manifest handoff. `LeanCapability` reads that manifest and keeps the build-time path,
dependency bundle, and initializer names together at runtime. Use `build_lake_target`, `LeanLibraryBundle`, and
`LeanLibrary` directly only for lower-level custom interop.

For sessions, kernel checking, and `MetaM`, add `lean-rs-host = "0.1"` and follow
[`crates/lean-rs-host/README.md`](crates/lean-rs-host/README.md). The host crate ships and builds its own shim packages;
your Lake package only declares your capability library.

## More worked examples

| Command | What it shows |
| --- | --- |
| `cargo run --manifest-path templates/shipped-lean-crate/Cargo.toml` | Canonical shipped-crate template: build-time Lean capability plus same-process Rust call. |
| `cargo run --manifest-path templates/shipped-lean-crate/Cargo.toml --example worker` | Canonical shipped worker template: app-owned worker child plus built Lean capability. |
| `cargo run -p lean-rs --example interop_callback` | Same-process Lean-to-Rust callback through `LeanCallbackHandle` without `lean-rs-host`. Recipe: [`downstream-interop.md`](docs/recipes/downstream-interop.md). |
| `cargo run -p lean-rs --example string_streaming` | Advanced same-process Lean-to-Rust string callbacks (`LeanCallbackHandle<LeanStringEvent>`). Recipe: [`string-callback-streaming.md`](docs/recipes/string-callback-streaming.md). |
| `cargo run -p lean-rs-worker --example worker_capability_runner` | Normal worker-capability path: builder, typed commands, live rows, diagnostics, timeouts, terminal completion, cycling. Recipe: [`worker-capability-runner.md`](docs/recipes/worker-capability-runner.md). |
| `cargo run -p lean-rs-worker --example worker_streaming` | Process-isolated typed streaming command with parent-side watchdog and worker cycling. Recipe: [`worker-process-boundary.md`](docs/recipes/worker-process-boundary.md). |
| `cargo run -p lean-rs-worker --example worker_pool` | Local multi-worker fanout via `LeanWorkerPool` and `LeanWorkerSessionLease`. |
| `cargo run --release -p lean-rs-worker --example worker_capability_probe` | Performance probe of generic command shapes (`version`, `doctor`, `extract`, `features`, `index`, `probe`). |
| `cargo run -p lean-rs-worker --example mathlib_scale_probe` | Planner → pool → session lease → typed command scale fixture. Set `LEAN_RS_MATHLIB_ROOT=/path/to/mathlib4` to use a real module list as the planning workload. |
| `cargo run -p lean-rs-worker --example lean_dup_readiness` | End-to-end readiness fixture exercising all command shapes through the import planner, pool, and lease. |

The Lean side of a worker capability can use `LeanRsInterop.Worker.Stream` helpers from `lean-rs-interop-shims` for row,
diagnostic, progress, terminal, and status envelopes. Downstream packages still own request parsing, row schemas,
semantic commands, and chunk contents.

## Recipes

| Recipe | When to use |
| --- | --- |
| [`ship-crate-with-lean.md`](docs/recipes/ship-crate-with-lean.md) | Canonical shipped crate: build-time Lean capability, runtime open helper, and app-owned worker child packaging. |
| [`downstream-interop.md`](docs/recipes/downstream-interop.md) | Rust-to-Lean exports plus same-process Lean-to-Rust callbacks without `lean-rs-host`. Advanced same-process path. |
| [`string-callback-streaming.md`](docs/recipes/string-callback-streaming.md) | Advanced same-process Lean-to-Rust string callbacks without `lean-rs-host`. |
| [`worker-capability-runner.md`](docs/recipes/worker-capability-runner.md) | Normal worker path: builder, typed commands, live rows, diagnostics, timeouts, cycling. |
| [`worker-process-boundary.md`](docs/recipes/worker-process-boundary.md) | Lower-level worker recipe: process isolation, memory cycling, row streaming. |

## Going deeper

> **Start here.** If you read only two architecture docs, read [`00-charter.md`](docs/architecture/00-charter.md)
> (design boundary, hidden knowledge, rejected alternatives) and
> [`01-safety-model.md`](docs/architecture/01-safety-model.md) (unsafe boundary, refcount ownership, concurrency
> stance). The rest are reference for specific topics. The numbers reflect the order docs were written, not the order
> they should be read.

Architecture, safety, and policy docs live under [`docs/`](docs/), grouped by topic:

**Foundations**
- [`00-charter.md`](docs/architecture/00-charter.md): design boundary, hidden knowledge, rejected alternatives.
- [`01-safety-model.md`](docs/architecture/01-safety-model.md): unsafe boundary, refcount ownership, concurrency stance.
- [`02-versioning-and-compatibility.md`](docs/architecture/02-versioning-and-compatibility.md): toolchain window, crate
  semver, supported platforms.
- [`05-raw-sys-design.md`](docs/architecture/05-raw-sys-design.md): per-decision rationale behind `lean-rs-sys`.

**Same-process FFI (`lean-rs`)**
- [`04-concurrency.md`](docs/architecture/04-concurrency.md): `!Send + !Sync` contract.
- [`06-panic-containment.md`](docs/architecture/06-panic-containment.md): panic containment via process boundary.
- [`07-cooperative-cancellation.md`](docs/architecture/07-cooperative-cancellation.md): cancellation token contract.
- [`08-reusable-interop.md`](docs/architecture/08-reusable-interop.md): reusable Lean/Rust interop boundary.
- [`09-callback-abi-spike.md`](docs/architecture/09-callback-abi-spike.md): callback ABI proof.
- [`10-callback-registry.md`](docs/architecture/10-callback-registry.md): RAII callback registry rules.
- [`11-generic-interop-shims.md`](docs/architecture/11-generic-interop-shims.md): reusable Lean-side interop shims.
- [`12-interop-build-and-link.md`](docs/architecture/12-interop-build-and-link.md): build-script helper path and cache.
- [`13-structured-progress.md`](docs/architecture/13-structured-progress.md): host progress-sink contract.
- [`14-interop-release-contract.md`](docs/architecture/14-interop-release-contract.md): interop release contract.
- [`15-callback-payloads.md`](docs/architecture/15-callback-payloads.md): sealed callback payload family.
- [`29-loader-and-artifact-boundary.md`](docs/architecture/29-loader-and-artifact-boundary.md): shipped-capability
  manifest, bundle loader, preflight, docs.rs/package, and worker bootstrap boundary.

**Host stack (`lean-rs-host`)**
- [`03-host-stack.md`](docs/architecture/03-host-stack.md): curated host surface and semver boundary.

**Worker (`lean-rs-worker`)**
- [`16-production-boundary.md`](docs/architecture/16-production-boundary.md): process boundary for fatal exits and
  memory reset.
- [`17-worker-session-adapter.md`](docs/architecture/17-worker-session-adapter.md): process-safe host-session subset.
- [`18-worker-data-streaming.md`](docs/architecture/18-worker-data-streaming.md): downstream JSON rows over the worker
  boundary.
- [`19-worker-capability-layer.md`](docs/architecture/19-worker-capability-layer.md): generic capability layer above raw
  rows.
- [`20-worker-pool.md`](docs/architecture/20-worker-pool.md): local pool and session-lease boundary.
- [`21-import-set-planning.md`](docs/architecture/21-import-set-planning.md): module discovery and batch planning.
- [`22-worker-row-batching.md`](docs/architecture/22-worker-row-batching.md): why row frames stay per-row.
- [`23-worker-data-plane-format.md`](docs/architecture/23-worker-data-plane-format.md): why the current row format
  stays.
- [`24-lean-side-worker-streaming.md`](docs/architecture/24-lean-side-worker-streaming.md): Lean-side envelope helpers.
- [`25-mathlib-scale-worker-fixture.md`](docs/architecture/25-mathlib-scale-worker-fixture.md): scale fixture and
  mathlib probe.
- [`26-worker-pool-observability.md`](docs/architecture/26-worker-pool-observability.md): pool snapshots and
  backpressure.
- [`27-lean-dup-readiness.md`](docs/architecture/27-lean-dup-readiness.md): readiness proof for subprocess-worker shape.
- [`28-production-scale-release.md`](docs/architecture/28-production-scale-release.md): local pool scale contract and
  non-goals.

Frozen public surfaces for each crate live under [`docs/api-review/`](docs/api-review/); later changes diff against
those baselines.

## Diagnostics

Every error-bearing public type projects to the stable [`LeanDiagnosticCode`](crates/lean-rs/src/error/mod.rs) taxonomy
via `.code()`, and the crates emit structured `tracing` spans against the `lean_rs` target. See
[`docs/diagnostics.md`](docs/diagnostics.md) for the code catalogue, span catalogue, recommended `RUST_LOG` scopes, and
the in-process `DiagnosticCapture` test affordance.

## Contributing

Workspace gates:

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo nextest run --workspace          # not `cargo test`; see docs/testing.md
cargo test --doc --workspace
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace
```

See [CONTRIBUTING.md](CONTRIBUTING.md) for contribution rules.

## License

Dual-licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or <http://www.apache.org/licenses/LICENSE-2.0>)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or <http://opensource.org/licenses/MIT>)

at your option.
