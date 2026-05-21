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

Eight host-stack examples live under
[`crates/lean-rs-host/examples/`](crates/lean-rs-host/examples/). Build the in-tree fixture
once, then run any of them:

```sh
(cd fixtures/lean && lake build)
cargo run -p lean-rs-host --example tour
```

`tour` composes the full host-stack flow (open → load capabilities → import session →
elaborate → kernel-check → bulk query → `Meta.whnf`) in one process; five focused examples
(`theorem_query`, `evaluate`, `proof_check`, `meta_query`, `progress`) each isolate one verb. See
[`crates/lean-rs-host/examples/README.md`](crates/lean-rs-host/examples/README.md) for the
per-example walkthrough—what each one teaches, expected output, and common failures.

For low-level L1 interop without `lean-rs-host`, run the generic callback
example:

```sh
cargo run -p lean-rs --example interop_callback
```

It builds the generic interop shim package and a downstream-style Lake target
through `lean-toolchain`, opens both dylibs, calls an ordinary Lean export from
Rust, and lets Lean call back into a Rust `LeanCallbackHandle`. This is an
advanced same-process mechanism for trusted extensions, not the worker-style
streaming interface. The worked recipe is
[`docs/recipes/downstream-interop.md`](docs/recipes/downstream-interop.md).
For the matching same-process string callback example, run:

```sh
cargo run -p lean-rs --example string_streaming
```

That example uses `LeanCallbackHandle<LeanStringEvent>` to receive owned
strings from Lean without importing `lean-rs-host`. Use the worker capability
recipe below when the caller needs process isolation, live rows, diagnostics,
timeouts, or memory cycling; see
[`docs/recipes/string-callback-streaming.md`](docs/recipes/string-callback-streaming.md).

For process isolation, fatal-child-exit reporting, memory cycling, and
downstream-owned row streaming, run the worker example:

```sh
cargo run -p lean-rs-worker --example worker_streaming
```

It starts a `lean-rs-worker` child, runs a typed streaming command, prints
JSONL-like rows projected from live typed row events, returns terminal row
counts / typed metadata separately from diagnostics, applies parent-owned
request watchdogs, exposes generic capability metadata and doctor checks,
cycles the worker, and proves the next request succeeds in a fresh child. The
example uses `LeanWorkerCapabilityBuilder`, so the caller does not hand-assemble
Lake output paths, worker child paths, or startup ordering. See
[`docs/recipes/worker-process-boundary.md`](docs/recipes/worker-process-boundary.md).

For the lean-dup-shaped worker capability fixture and local performance probe,
run:

```sh
cargo run --release -p lean-rs-worker --example worker_capability_probe
```

It exercises generic `version`, `doctor`, `extract`, `features`, `index`, and
`probe` command shapes without importing downstream schemas into
`lean-rs-worker`.

For the source-of-truth worker capability recipe, run:

```sh
cargo run -p lean-rs-worker --example worker_capability_runner
```

It demonstrates the normal downstream path: `LeanWorkerCapabilityBuilder`,
typed commands, live rows, diagnostics, progress ticks, terminal completion,
request timeout handling, and worker cycling. See
[`docs/recipes/worker-capability-runner.md`](docs/recipes/worker-capability-runner.md).

For local multi-worker orchestration, use `LeanWorkerPool`:

```sh
cargo run -p lean-rs-worker --example worker_pool
```

The pool acquires `LeanWorkerSessionLease` values from capability requirements
and runs typed commands through the lease. It hides child selection, warm
worker reuse, replacement after fatal exits, lease invalidation, and fixed
local worker admission. Session keys are worker reuse keys, not downstream
cache keys.

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
    lean_toolchain::emit_lean_link_directives_checked()?;
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
    println!("{}", add.call(40, 2)?);  // 42
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
[`crates/lean-rs-host/README.md`](crates/lean-rs-host/README.md). The host crate ships and
builds its own shim packages; your Lake package only declares your capability library.

## The five published crates

`lean-rs-sys` is the raw Lean 4 C ABI binding: curated `extern "C"` declarations split by
semantic category, pure-Rust mirrors of `lean.h`'s `static inline` refcount helpers, the
`REQUIRED_SYMBOLS` allowlist, and the header digest. Public types (`lean_object`) are opaque;
layout is `pub(crate)`. Opt-in unsafe raw FFI; the safe layers in `lean-rs` are the recommended
path.

`lean-toolchain` provides Lean toolchain discovery, the typed `ToolchainFingerprint`, fixture
digest, layered link diagnostics, and `build.rs` helpers downstream embedders call from their
own build scripts (`emit_lean_link_directives_checked` and `build_lake_target`, used above).

**`lean-rs` is the L1 FFI primitive.** Runtime initialization (token-bound `'lean` lifetime),
owned/borrowed object handles, typed ABI conversions, module loading, typed exported
functions, semantic handles (`LeanName`/`LeanLevel`/`LeanExpr`/`LeanDeclaration`), RAII
callback registrations for same-process Lean-to-Rust calls, and a structured error/diagnostic
boundary. Ships the generic interop shim package used by callbacks, but no theorem-prover host
shims. Callback handles are an L1 mechanism; worker-style applications should use
`lean-rs-worker` typed commands instead of passing callback handles through their own APIs.

**`lean-rs-host` is the L2 opinionated host stack.** The `LeanHost` / `LeanCapabilities` /
`LeanSession` trio, kernel-checked `LeanEvidence` and `ProofSummary`, the bounded `MetaM`
service registry, and `SessionPool` / `PooledSession`. Requires the 26 + 4 `lean_rs_host_*`
Lean shim contract shipped with the crate and loaded alongside the consumer capability dylib.
Long-running calls can report live progress through `LeanProgressSink`.

**`lean-rs-worker` is the process-boundary worker stack.** It supervises a
`lean-rs-worker-child` process around `lean-rs-host` for fatal-exit containment
and memory cycling. `LeanWorkerCapabilityBuilder` is the normal downstream
entry point: it builds the Lake target, starts the worker, opens imports,
optionally validates metadata, and leaves request/row schemas to the downstream
crate. `LeanWorkerPool` sits above the builder for local fanout and session
leasing; callers still use typed commands instead of choosing worker children
or protocol frames.

Compose at the highest layer that matches the workload. Use `lean-rs` for
custom same-process ABI work, `lean-rs-host` for trusted in-process
theorem-prover sessions, and `lean-rs-worker` for production worker-style tools
that need process isolation, streaming rows, request timeouts, or memory
cycling. Lower layers remain available as escape hatches, but ordinary
applications should not manually wire callbacks, sessions, pipes, and restart
policy when a higher layer already owns that work.

The in-process layering invariant is
`lean-rs-sys` → `lean-toolchain` → `lean-rs` → `lean-rs-host`;
`lean-rs-worker` wraps that host stack in a child-process boundary. Raw
`lean_object *` and raw `lean_*` symbols enter the workspace only via
`lean-rs-sys` and are not re-exported by `lean-toolchain` or `lean-rs`. The L1
(`lean-rs`) curated surface is the typed FFI primitive plus the four core
semantic handle types and the error boundary; the L2 (`lean-rs-host`) curated
surface is the opinionated theorem-prover-host capability stack. See
[`docs/architecture/03-host-stack.md`](docs/architecture/03-host-stack.md) for
the L2 classification table.

## Going deeper

Architecture and policy docs live under [`docs/architecture/`](docs/architecture/):

- [`00-charter.md`](docs/architecture/00-charter.md)—design boundary, hidden knowledge, smallest public interface, rejected alternatives.
- [`01-safety-model.md`](docs/architecture/01-safety-model.md)—unsafe boundary, reference-counting ownership, proof-object opacity, concurrency stance, and the workspace `unsafe-code = "deny"` policy.
- [`02-versioning-and-compatibility.md`](docs/architecture/02-versioning-and-compatibility.md)—supported Lean toolchain window, in-tree raw-FFI policy, crate semver, supported platforms.
- [`03-host-stack.md`](docs/architecture/03-host-stack.md)—the curated `lean-rs-host` surface and its semver boundary.
- [`04-concurrency.md`](docs/architecture/04-concurrency.md)—the `!Send + !Sync` contract and worker-thread attach discipline.
- [`05-raw-sys-design.md`](docs/architecture/05-raw-sys-design.md)—per-decision rationale behind `lean-rs-sys`.
- [`06-panic-containment.md`](docs/architecture/06-panic-containment.md)—why Lean internal panics are contained by process boundaries, not `LeanSession` poisoning.
- [`07-cooperative-cancellation.md`](docs/architecture/07-cooperative-cancellation.md)—the token-based cancellation contract and its non-preemptive limits.
- [`08-reusable-interop.md`](docs/architecture/08-reusable-interop.md)—the reusable Lean/Rust interop boundary below `lean-rs-host`.
- [`09-callback-abi-spike.md`](docs/architecture/09-callback-abi-spike.md)—the test-only callback ABI proof before a public callback registry.
- [`10-callback-registry.md`](docs/architecture/10-callback-registry.md)—the low-level L1 RAII callback registry and its panic, lifetime, and reentrancy rules.
- [`11-generic-interop-shims.md`](docs/architecture/11-generic-interop-shims.md)—the reusable Lean-side interop shim package.
- [`12-interop-build-and-link.md`](docs/architecture/12-interop-build-and-link.md)—the downstream build-script helper path and cache/diagnostic contract.
- [`13-structured-progress.md`](docs/architecture/13-structured-progress.md)—the host progress-sink contract over the reusable callback substrate.
- [`14-interop-release-contract.md`](docs/architecture/14-interop-release-contract.md)—the final interop release contract and source-of-truth map.
- [`15-callback-payloads.md`](docs/architecture/15-callback-payloads.md)—the sealed typed callback payload family.
- [`16-production-boundary.md`](docs/architecture/16-production-boundary.md)—the worker-process boundary for fatal exits and memory reset.
- [`17-worker-session-adapter.md`](docs/architecture/17-worker-session-adapter.md)—the narrow process-safe host-session subset.
- [`18-worker-data-streaming.md`](docs/architecture/18-worker-data-streaming.md)—arbitrary downstream JSON rows over the worker boundary.
- [`19-worker-capability-layer.md`](docs/architecture/19-worker-capability-layer.md)—the generic worker capability layer above raw row transport.
- [`20-worker-pool.md`](docs/architecture/20-worker-pool.md)—the local worker pool and session-lease boundary.
- [`21-import-set-planning.md`](docs/architecture/21-import-set-planning.md)—Lake module discovery and worker session batch planning.
- [`22-worker-row-batching.md`](docs/architecture/22-worker-row-batching.md)—the measured decision not to add row-batch frames or a public batch sink yet.
- [`downstream-interop.md`](docs/recipes/downstream-interop.md)—the advanced L1 recipe for Rust-to-Lean exported calls and same-process Lean-to-Rust callbacks without `lean-rs-host`.
- [`string-callback-streaming.md`](docs/recipes/string-callback-streaming.md)—the advanced L1 recipe for same-process Lean-to-Rust string callbacks.
- [`worker-process-boundary.md`](docs/recipes/worker-process-boundary.md)—the worker recipe for process isolation, memory cycling, and downstream row streaming.
- [`worker-capability-runner.md`](docs/recipes/worker-capability-runner.md)—the normal worker-capability recipe with builder setup, typed commands, live rows, diagnostics, timeout handling, terminal completion, and worker cycling.

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
