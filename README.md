# lean-rs

[![CI](https://github.com/jcreinhold/lean-rs/actions/workflows/ci.yml/badge.svg)](https://github.com/jcreinhold/lean-rs/actions/workflows/ci.yml)
[![Sanitizer](https://github.com/jcreinhold/lean-rs/actions/workflows/sanitizer.yml/badge.svg)](https://github.com/jcreinhold/lean-rs/actions/workflows/sanitizer.yml)
[![Release](https://github.com/jcreinhold/lean-rs/actions/workflows/release.yml/badge.svg)](https://github.com/jcreinhold/lean-rs/actions/workflows/release.yml)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT_OR_Apache--2.0-blue.svg)](#license)

Rust bindings for hosting [Lean 4](https://lean-lang.org/) capabilities. Lean owns Lean
semantics (elaboration, kernel checking, proof objects, universes, `MetaM`, dependent-type
meaning). This project owns hosting: linking, runtime initialization, ABI conversion, module
loading, error and panic boundaries, scheduling, diagnostics, batching, and packaging. Rust
does not reconstruct Lean semantic facts.

## Prerequisites

- [`elan`](https://github.com/leanprover/elan) and a Lean 4 toolchain in the
  [supported window](docs/version-matrix.md) (currently 4.26.0–4.29.1). `elan` ships `lean`
  and `lake` together.
- Rust stable (MSRV 1.91).
- macOS or Linux. Windows is not supported.

## Run an example

```sh
(cd fixtures/lean && lake build)
cargo run -p lean-rs-host --example tour
```

`tour` composes the full host-stack flow in one process: open the runtime, load capabilities,
open an import session, elaborate, kernel-check, run a bulk query, and call `Meta.whnf`. See
[`crates/lean-rs-host/examples/README.md`](crates/lean-rs-host/examples/README.md) for the
per-example walkthrough.

## The five published crates

| Crate            | Role |
| ---------------- | ---- |
| `lean-rs-sys`    | Raw Lean 4 C ABI. Opaque public types, refcount mirrors, `REQUIRED_SYMBOLS` allowlist, header digest. Opt-in unsafe raw FFI. |
| `lean-toolchain` | Toolchain discovery, fingerprint, fixture digest, link diagnostics, `build.rs` helpers. |
| `lean-rs`        | **L1.** Safe FFI primitive: runtime, object handles, ABI conversions, module loading, exported functions, semantic handles, callbacks, error boundary. |
| `lean-rs-host`   | **L2.** Theorem-prover-host stack on top of `lean-rs`: `LeanHost` / `LeanCapabilities` / `LeanSession`, kernel-checked evidence, bounded `MetaM`, session pool. |
| `lean-rs-worker` | Process-boundary supervisor around `lean-rs-host`: child lifecycle, request timeouts, memory cycling, typed commands, row streaming, local pool. |

Layering: `lean-rs-sys` → `lean-toolchain` → `lean-rs` → `lean-rs-host`. `lean-rs-worker`
wraps the host stack in a child-process boundary. Raw `lean_*` symbols enter the workspace
only through `lean-rs-sys`; the safe layers never re-export them.

Compose at the highest layer that fits the workload:

- `lean-rs` for custom same-process ABI calls, module loading, and callbacks.
- `lean-rs-host` for trusted in-process theorem-prover work: imports, elaboration, kernel
  checks, declaration queries, progress, cancellation, pooling.
- `lean-rs-worker` for production tools needing process isolation, streaming rows, request
  timeouts, or memory cycling.

Lower layers are escape hatches, not steps every downstream caller should hand-compose. See
[`docs/architecture/03-host-stack.md`](docs/architecture/03-host-stack.md) for the L2
classification table.

## Build your own consumer

The minimum L1 setup is five files. The example calls a user-authored `@[export]` Lean
function from Rust without depending on `lean-rs-host`.

**`Cargo.toml`**: `lean-rs` for the API; `lean-toolchain` is a build-dep that emits link
directives and the runtime rpath:

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

**`build.rs`**: one helper covers link-search, link-lib, and the runtime rpath into the
Lean toolchain's `lib/lean` directory; the other builds the Lake shared-library target and
records the dylib path for `main.rs`:

```rust
use std::path::Path;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    lean_toolchain::emit_lean_link_directives_checked()?;
    let dylib = lean_toolchain::build_lake_target(Path::new("lean"), "MyCapability")?;
    println!("cargo:rustc-env=MY_CAPABILITY_DYLIB={}", dylib.display());
    Ok(())
}
```

**`lean/lakefile.lean`**: minimal Lake package emitting a shared library:

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

**`src/main.rs`**: open the dylib, dispatch typed:

```rust
use lean_rs::{LeanLibrary, LeanResult, LeanRuntime};

fn main() -> LeanResult<()> {
    let runtime = LeanRuntime::init()?;
    let library = LeanLibrary::open(runtime, env!("MY_CAPABILITY_DYLIB"))?;
    let module = library.initialize_module("my_app", "MyCapability")?;

    let add = module.exported::<(u64, u64), u64>("my_app_add")?;
    println!("{}", add.call(40, 2)?);  // 42
    Ok(())
}
```

Build and run:

```sh
cargo run
```

`build_lake_target` hides Lake's shared-library facet, cache, and filename convention.
`initialize_module(package, lib_name)` takes the unmangled Lake names.

For the L2 path (sessions, kernel checking, `MetaM`), add `lean-rs-host = "0.1"` and follow
[`crates/lean-rs-host/README.md`](crates/lean-rs-host/README.md). The host crate ships and
builds its own shim packages; your Lake package only declares your capability library.

## More worked examples

| Command | What it shows |
| ------- | ------------- |
| `cargo run -p lean-rs --example interop_callback` | Same-process Lean-to-Rust callback through `LeanCallbackHandle` without `lean-rs-host`. Recipe: [`downstream-interop.md`](docs/recipes/downstream-interop.md). |
| `cargo run -p lean-rs --example string_streaming` | Same-process Lean-to-Rust string callbacks (`LeanCallbackHandle<LeanStringEvent>`). Recipe: [`string-callback-streaming.md`](docs/recipes/string-callback-streaming.md). |
| `cargo run -p lean-rs-worker --example worker_capability_runner` | Normal worker-capability path: builder, typed commands, live rows, diagnostics, timeouts, terminal completion, cycling. Recipe: [`worker-capability-runner.md`](docs/recipes/worker-capability-runner.md). |
| `cargo run -p lean-rs-worker --example worker_streaming` | Process-isolated typed streaming command with parent-side watchdog and worker cycling. Recipe: [`worker-process-boundary.md`](docs/recipes/worker-process-boundary.md). |
| `cargo run -p lean-rs-worker --example worker_pool` | Local multi-worker fanout via `LeanWorkerPool` and `LeanWorkerSessionLease`. |
| `cargo run --release -p lean-rs-worker --example worker_capability_probe` | Performance probe of generic command shapes (`version`, `doctor`, `extract`, `features`, `index`, `probe`). |
| `cargo run -p lean-rs-worker --example mathlib_scale_probe` | Planner → pool → session lease → typed command scale fixture. Set `LEAN_RS_MATHLIB_ROOT=/path/to/mathlib4` to use a real module list as the planning workload. |
| `cargo run -p lean-rs-worker --example lean_dup_readiness` | End-to-end readiness fixture exercising all command shapes through the import planner, pool, and lease. |

The Lean side of a worker capability can use `LeanRsInterop.Worker.Stream` helpers from
`lean-rs-interop-shims` for row, diagnostic, progress, terminal, and status envelopes.
Downstream packages still own request parsing, row schemas, semantic commands, and chunk
contents.

## Going deeper

Architecture, safety, and policy docs live under [`docs/`](docs/). The list below groups
them by topic; numbering reflects the historical order, not the reading order.

**Foundations**
- [`00-charter.md`](docs/architecture/00-charter.md): design boundary, hidden knowledge, rejected alternatives.
- [`01-safety-model.md`](docs/architecture/01-safety-model.md): unsafe boundary, refcount ownership, concurrency stance.
- [`02-versioning-and-compatibility.md`](docs/architecture/02-versioning-and-compatibility.md): toolchain window, crate semver, supported platforms.
- [`05-raw-sys-design.md`](docs/architecture/05-raw-sys-design.md): per-decision rationale behind `lean-rs-sys`.

**L1 surface (`lean-rs`)**
- [`04-concurrency.md`](docs/architecture/04-concurrency.md): `!Send + !Sync` contract.
- [`06-panic-containment.md`](docs/architecture/06-panic-containment.md): panic containment via process boundary.
- [`07-cooperative-cancellation.md`](docs/architecture/07-cooperative-cancellation.md): cancellation token contract.
- [`08-reusable-interop.md`](docs/architecture/08-reusable-interop.md): reusable Lean/Rust interop boundary.
- [`09-callback-abi-spike.md`](docs/architecture/09-callback-abi-spike.md): callback ABI proof.
- [`10-callback-registry.md`](docs/architecture/10-callback-registry.md): L1 RAII callback registry rules.
- [`11-generic-interop-shims.md`](docs/architecture/11-generic-interop-shims.md): reusable Lean-side interop shims.
- [`12-interop-build-and-link.md`](docs/architecture/12-interop-build-and-link.md): build-script helper path and cache.
- [`13-structured-progress.md`](docs/architecture/13-structured-progress.md): host progress-sink contract.
- [`14-interop-release-contract.md`](docs/architecture/14-interop-release-contract.md): interop release contract.
- [`15-callback-payloads.md`](docs/architecture/15-callback-payloads.md): sealed callback payload family.

**L2 host stack (`lean-rs-host`)**
- [`03-host-stack.md`](docs/architecture/03-host-stack.md): curated host surface and semver boundary.

**Worker (`lean-rs-worker`)**
- [`16-production-boundary.md`](docs/architecture/16-production-boundary.md): process boundary for fatal exits and memory reset.
- [`17-worker-session-adapter.md`](docs/architecture/17-worker-session-adapter.md): process-safe host-session subset.
- [`18-worker-data-streaming.md`](docs/architecture/18-worker-data-streaming.md): downstream JSON rows over the worker boundary.
- [`19-worker-capability-layer.md`](docs/architecture/19-worker-capability-layer.md): generic capability layer above raw rows.
- [`20-worker-pool.md`](docs/architecture/20-worker-pool.md): local pool and session-lease boundary.
- [`21-import-set-planning.md`](docs/architecture/21-import-set-planning.md): module discovery and batch planning.
- [`22-worker-row-batching.md`](docs/architecture/22-worker-row-batching.md): why row frames stay per-row.
- [`23-worker-data-plane-format.md`](docs/architecture/23-worker-data-plane-format.md): why the current row format stays.
- [`24-lean-side-worker-streaming.md`](docs/architecture/24-lean-side-worker-streaming.md): Lean-side envelope helpers.
- [`25-mathlib-scale-worker-fixture.md`](docs/architecture/25-mathlib-scale-worker-fixture.md): scale fixture and mathlib probe.
- [`26-worker-pool-observability.md`](docs/architecture/26-worker-pool-observability.md): pool snapshots and backpressure.
- [`27-lean-dup-readiness.md`](docs/architecture/27-lean-dup-readiness.md): readiness proof for subprocess-worker shape.
- [`28-production-scale-release.md`](docs/architecture/28-production-scale-release.md): local pool scale contract and non-goals.

**Recipes**
- [`downstream-interop.md`](docs/recipes/downstream-interop.md): Rust-to-Lean exports + same-process callbacks without `lean-rs-host`.
- [`string-callback-streaming.md`](docs/recipes/string-callback-streaming.md): same-process Lean-to-Rust string callbacks.
- [`worker-process-boundary.md`](docs/recipes/worker-process-boundary.md): process isolation, memory cycling, row streaming.
- [`worker-capability-runner.md`](docs/recipes/worker-capability-runner.md): worker builder, typed commands, live rows, timeouts, cycling.

Frozen public surfaces for each crate live under [`docs/api-review/`](docs/api-review/);
later changes diff against those baselines.

## Diagnostics

Every error-bearing public type projects to the stable
[`LeanDiagnosticCode`](crates/lean-rs/src/error/mod.rs) taxonomy via `.code()`, and the
crates emit structured `tracing` spans against the `lean_rs` target. See
[`docs/diagnostics.md`](docs/diagnostics.md) for the code catalogue, span catalogue,
recommended `RUST_LOG` scopes, and the in-process `DiagnosticCapture` test affordance.

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
