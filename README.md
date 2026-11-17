# lean-rs

[![CI](https://github.com/jcreinhold/lean-rs/actions/workflows/ci.yml/badge.svg)](https://github.com/jcreinhold/lean-rs/actions/workflows/ci.yml)
[![Sanitizer](https://github.com/jcreinhold/lean-rs/actions/workflows/sanitizer.yml/badge.svg)](https://github.com/jcreinhold/lean-rs/actions/workflows/sanitizer.yml)
[![Release](https://github.com/jcreinhold/lean-rs/actions/workflows/release.yml/badge.svg)](https://github.com/jcreinhold/lean-rs/actions/workflows/release.yml)
[![Docs](https://img.shields.io/badge/docs-jcreinhold.github.io%2Flean--rs-blue)](https://jcreinhold.github.io/lean-rs/)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT_OR_Apache--2.0-blue.svg)](#license)

A Lean 4 runtime bridge for Rust: load Lean libraries, call typed `@[export]` functions, use elaboration and kernel
checking, or isolate Lean work in supervised child processes. Lean owns dependent-type meaning and proof checking; Rust
owns linking, object conversion, scheduling, diagnostics, and packaging.

## Requirements

- [`elan`](https://github.com/leanprover/elan) with a Lean toolchain in the [supported window](docs/version-matrix.md).
- Stable Rust, MSRV 1.94 (declared in [Cargo.toml](Cargo.toml)).
- macOS or Linux.

## Start with a runnable project

The [shipped-crate template](templates/shipped-lean-crate/) builds its Lean library from `build.rs` and calls it from
Rust. It also includes an application-owned worker binary. Shared-source build helpers require a configured writer
helper through `LEAN_HOST_MCP_WRITER_EXECUTABLE`; the in-tree template does not configure one itself. See the
[build prerequisites](docs/recipes/ship-crate-with-lean.md) before running:

```sh
cargo run --manifest-path templates/shipped-lean-crate/Cargo.toml
cargo build --manifest-path templates/shipped-lean-crate/Cargo.toml --bin shipped-lean-crate-worker
cargo run --manifest-path templates/shipped-lean-crate/Cargo.toml --example worker
```

Follow [Ship a crate with Lean](docs/recipes/ship-crate-with-lean.md) for the dependency declarations, Lean source,
export signatures, and runtime manifest handoff. For a custom same-process integration, use
[Downstream interop](docs/recipes/downstream-interop.md).

To explore the standard host services in this checkout:

```sh
(cd fixtures/lean && lake build)
cargo run -p lean-rs-host --example tour
```

The tour opens an import session, elaborates a term, kernel-checks it, runs a bulk query, and calls `Meta.whnf`. See the
[host crate](crates/lean-rs-host/README.md) for service APIs. For long-running import-heavy services, follow
[Production hosting](docs/production-hosting.md) for bounded workers, warm sessions, and process cycling.

## Choose a crate

All published crates share the workspace version; dependency examples use `"0.9"` for the 0.9.x series.

| Job | Crate |
| --- | --- |
| Call typed Lean exports, load modules, or receive callbacks in one process | [`lean-rs`](crates/lean-rs/README.md) |
| Use imports, elaboration, kernel checks, declaration queries, or bounded `MetaM` | [`lean-rs-host`](crates/lean-rs-host/README.md) |
| Supervise child processes, pool sessions, or receive typed row streams | [`lean-rs-worker-parent`](crates/lean-rs-worker-parent/README.md) |
| Build the Lean-linked worker binary | [`lean-rs-worker-child`](crates/lean-rs-worker-child/README.md) |
| Implement a worker protocol peer | [`lean-rs-worker-protocol`](crates/lean-rs-worker-protocol/README.md) |
| Discover toolchains or build and package Lean capabilities | [`lean-toolchain`](crates/lean-toolchain/README.md) |
| Read static ABI metadata without Lean installed | [`lean-rs-abi`](crates/lean-rs-abi/README.md) |
| Materialize the reusable Lean callback and streaming shims | [`lean-rs-interop-shims`](crates/lean-rs/shims/lean-rs-interop-shims/README.md) |
| Call raw Lean C symbols with explicit unsafe obligations | [`lean-rs-sys`](crates/lean-rs-sys/README.md) |

The safe layers hide Lean object layout, reference counting, and raw dispatch. Checked export lookup relies on trusted
ABI metadata; arbitrary symbol lookup remains explicitly unsafe. The
[safety model](docs/architecture/01-safety-model.md) records the required guarantees and current boundary gaps; Lean
remains responsible for semantic correctness. Resource exhaustion and fatal Lean exits require a process boundary. Read
the [charter](docs/architecture/00-charter.md) and [safety model](docs/architecture/01-safety-model.md) before changing
these boundaries.

## Documentation and examples

The [documentation index](docs/SUMMARY.md) is the complete reading map; the same pages are published with search at
<https://jcreinhold.github.io/lean-rs/>.

| Task | Guide or example |
| --- | --- |
| Receive Lean-to-Rust callbacks | [Downstream interop](docs/recipes/downstream-interop.md) |
| Run typed commands with live rows, diagnostics, and deadlines | [Worker capability runner](docs/recipes/worker-capability-runner.md) |
| Work with raw worker requests and frames | [Worker transport](docs/recipes/worker-capability-runner.md#wire-and-raw-rows) |
| Fan out through a local pool | `cargo run -p lean-rs-worker-child --example worker_pool` |
| Inspect import planning and scale probes | [Scale fixtures](docs/architecture/25-mathlib-scale-worker-fixture.md) |
| Diagnose a failure | [Diagnostics](docs/diagnostics.md); start with `RUST_LOG=lean_rs=debug` |
| Measure performance or memory | [Performance](docs/performance.md) and [retention measurements](docs/safety/long-session-memory.md) |

Public API snapshots live in [docs/api-review/](docs/api-review/). [CONTRIBUTING.md](CONTRIBUTING.md) describes local
checks, CI coverage, and review requirements.

## License

Dual-licensed under [Apache-2.0](LICENSE-APACHE) or [MIT](LICENSE-MIT), at your option.
