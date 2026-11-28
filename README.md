# lean-rs

[![CI](https://github.com/jcreinhold/lean-rs/actions/workflows/ci.yml/badge.svg)](https://github.com/jcreinhold/lean-rs/actions/workflows/ci.yml)
[![Sanitizer](https://github.com/jcreinhold/lean-rs/actions/workflows/sanitizer.yml/badge.svg)](https://github.com/jcreinhold/lean-rs/actions/workflows/sanitizer.yml)
[![Release](https://github.com/jcreinhold/lean-rs/actions/workflows/release.yml/badge.svg)](https://github.com/jcreinhold/lean-rs/actions/workflows/release.yml)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT_OR_Apache--2.0-blue.svg)](#license)

lean-rs runs Lean 4 from Rust. It finds a Lean toolchain, asks Lake for paths, loads Lean libraries, imports modules,
elaborates, evaluates and kernel-checks, and runs that work in worker processes that exit when their parent dies. Lean
decides what terms mean; lean-rs handles linking, object conversion, processes and diagnostics.
[docs/architecture.md](docs/architecture.md) states the goal, who owns what, and each invariant with its test.

## Install

You need [elan](https://github.com/leanprover/elan) with a Lean release from the [version matrix](#version-matrix),
stable Rust 1.94 or later, and x86_64 Linux or Apple Silicon macOS.

The parent process depends on `lean-rs-worker-parent`, which does not link Lean:

```toml
[dependencies]
lean-rs-worker-parent = "0.13"
```

The worker is a separate binary that links Lean. Its crate depends on `lean-rs-worker-child = "0.13"`, and its `main` is
one call:

```rust
fn main() -> std::process::ExitCode {
    lean_rs_worker_child::run_worker_child_stdio()
}
```

## Example

Elaborate a term in a worker:

```rust
use lean_rs_worker_parent::{LeanWorkerElabOptions, LeanWorkerError, LeanWorkerHostHandleBuilder};

fn main() -> Result<(), LeanWorkerError> {
    let mut host = LeanWorkerHostHandleBuilder::shims_only("path/to/lake/project", ["Init"])
        .worker_executable("path/to/your-worker")
        .open()?;
    {
        let mut session = host.open_session(None, None)?;
        let result = session.elaborate("(2 + 2 : Nat)", &LeanWorkerElabOptions::default(), None, None)?;
        println!("ok: {}, diagnostics: {}", result.success, result.diagnostics.len());
    }
    host.shutdown()?;
    Ok(())
}
```

The session borrows the handle, so it must end before `shutdown`. The first session builds the host shims with Lake,
which takes a while.

## Crates

All published crates share one version.

| Crate | What it does |
| --- | --- |
| [`lean-rs-worker-parent`](crates/lean-rs-worker-parent/README.md) | Starts and supervises worker processes; no Lean link |
| [`lean-rs-worker-child`](crates/lean-rs-worker-child/README.md) | The Lean-linked worker binary |
| [`lean-rs-worker-protocol`](crates/lean-rs-worker-protocol/README.md) | The messages between parent and worker |
| [`lean-rs-host`](crates/lean-rs-host/README.md) | Imports, elaboration, kernel checks and queries in one process |
| [`lean-rs`](crates/lean-rs/README.md) | The Lean runtime, object ownership, loading and callbacks |
| [`lean-toolchain`](crates/lean-toolchain/README.md) | Toolchain discovery, Lake queries, build helpers and process groups |
| [`lean-rs-sys`](crates/lean-rs-sys/README.md) | Raw `lean.h` bindings |
| [`lean-rs-abi`](crates/lean-rs-abi/README.md) | The supported toolchain window and ABI constants, without Lean |

## Version matrix

| Setting | Supported |
| --- | --- |
| Lean | 4.30.0; 4.31.0-rc1, 4.31.0-rc2, 4.31.0; 4.32.0-rc1, 4.32.0, 4.32.2; 4.33.0-rc1, 4.33.0-rc2, 4.33.0, 4.33.1; 4.34.0-rc1, 4.34.0-rc2, 4.34.0, 4.34.1; 4.35.0-rc1, 4.35.0-rc2, 4.35.0-rc3, 4.35.0-rc4, 4.35.0; 4.36.0 |
| Rust | Stable, 1.94 or later |
| Platforms | `x86_64-unknown-linux-gnu`, `aarch64-apple-darwin` |

A Lean release not in the list is unsupported even if it builds. The listed releases ship 13 distinct `lean.h` headers,
and each exports all 91 symbols lean-rs requires. The build checks the active header against
[`SUPPORTED_TOOLCHAINS`](crates/lean-rs-abi/src/supported.rs).

The window starts at 4.30.0. Releases 4.27.0 to 4.29.1 do not export the `collectAndGet` symbol that the host shims
reach through `Lean.collectAxioms`; 4.26 fails to build the shims; 4.25 and earlier crash in `lean_dec_ref_cold`.

Pushes, pull requests and release builds test the newest release, 4.36.0, on `ubuntu-latest` and `macos-latest`. A CI
run started with `full_matrix: true` tests every listed release on both.

## License

Dual-licensed under [Apache-2.0](LICENSE-APACHE) or [MIT](LICENSE-MIT), at your option.
