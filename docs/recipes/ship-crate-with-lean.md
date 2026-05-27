# Ship A Crate With Lean Code

Use this recipe when a Rust crate owns Lean source code and should build on another developer's machine with ordinary
Cargo commands.

## Run The Template

From the repository root:

```sh
cargo run --manifest-path templates/shipped-lean-crate/Cargo.toml
cargo build --manifest-path templates/shipped-lean-crate/Cargo.toml --bin shipped-lean-crate-worker
cargo run --manifest-path templates/shipped-lean-crate/Cargo.toml --example worker
```

The first command builds the Lean shared library in `build.rs` and calls a Lean export in process. The second builds the
app-owned worker child binary. The third starts that worker child and opens the same capability behind the worker
crates.

The template uses path dependencies because it lives inside this repository. Published crates use normal version
dependencies:

```toml
[dependencies]
lean-rs = "0.1"
lean-rs-worker = "0.1" # only for worker apps

[build-dependencies]
lean-toolchain = "0.1"
```

## File Layout

The canonical shape is build-time first:

1. `build.rs` builds the Lake shared-library target.
2. Rust opens the built dylib through `lean-rs`, or starts it behind the worker crates.
3. Worker applications ship their own tiny worker-child binary.

The crate author names the Lake root, package, module, imports, and worker child binary. The helper APIs hide Lean link
directives, Lake output paths, dylib env-var plumbing, worker protocol frames, pipes, and child startup sequencing.

## Build The Lean Capability

Add `lean-toolchain` as a build dependency:

```toml
[build-dependencies]
lean-toolchain = "0.1"
```

Use the high-level helper in `build.rs`:

```rust,ignore
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use lean_toolchain::{
        LeanExportAbiRepr, LeanExportArgAbi, LeanExportOwnership, LeanExportResultConvention, LeanExportReturnAbi,
        LeanExportSignature,
    };

    lean_toolchain::CargoLeanCapability::new("lean", "MyCapability")
        .package("my_app")
        .module("MyCapability")
        .export_signature(LeanExportSignature::function(
            "my_app_add",
            vec![
                LeanExportArgAbi::new(LeanExportAbiRepr::U64, LeanExportOwnership::None),
                LeanExportArgAbi::new(LeanExportAbiRepr::U64, LeanExportOwnership::None),
            ],
            LeanExportReturnAbi::new(
                LeanExportAbiRepr::U64,
                LeanExportOwnership::None,
                LeanExportResultConvention::Pure,
            ),
        ))
        .build()?;
    Ok(())
}
```

This emits Lean link directives, Cargo rerun triggers, runs `lake build MyCapability:shared`, resolves Lake's supported
dylib naming conventions, writes a JSON artifact manifest with trusted export signature metadata, and emits:

```text
cargo:rustc-env=LEAN_RS_CAPABILITY_MY_CAPABILITY_MANIFEST=<manifest path>
cargo:rustc-env=LEAN_RS_CAPABILITY_MY_CAPABILITY_DYLIB=<built dylib path>
```

The manifest path is the canonical runtime handoff. The dylib env var remains for compatibility and simple low-level
callers.

Use `build_lake_target` directly only when a crate really needs lower-level control over the build-script policy.

## Open The Capability In Process

For trusted same-process interop, embed the compile-time path with `env!`:

```rust,ignore
let runtime = lean_rs::LeanRuntime::init()?;
let capability = lean_rs::LeanCapability::from_build_manifest(
    runtime,
    lean_rs::LeanBuiltCapability::manifest_path(
        env!("LEAN_RS_CAPABILITY_MY_CAPABILITY_MANIFEST"),
    ),
)?;

let add = capability.exported::<(u64, u64), u64>("my_app_add")?;
let answer = add.call(40, 2)?;
```

`LeanCapability` is a convenience layer over `LeanLibrary`: it reads the manifest, opens the primary dylib and
dependency bundle, initializes the configured module, and keeps the initializer names and trusted export signatures with
the opened library. `LeanCapability::exported` fails before dispatch when the manifest lacks `my_app_add` or records a
different ABI shape. For doctor commands or installer checks, run `LeanCapabilityPreflight` against the same manifest
descriptor first; it reports missing package files, unsupported toolchain fingerprints, stale manifests, and missing
initializers with stable loader codes and repair hints.

## Run The Capability In A Worker

Worker applications should ship an app-owned child binary. Do not rely on a dependency binary being installed
automatically.

```rust,ignore
// src/bin/my_app_lean_worker.rs
fn main() -> std::process::ExitCode {
    lean_rs_worker::run_worker_child_stdio()
}
```

Then point the worker builder at that binary:

```rust,ignore
let spec = lean_rs::LeanBuiltCapability::manifest_path(
    env!("LEAN_RS_CAPABILITY_MY_CAPABILITY_MANIFEST"),
)
.manifest_env_var("LEAN_RS_CAPABILITY_MY_CAPABILITY_MANIFEST");

let builder =
    lean_rs_worker::LeanWorkerCapabilityBuilder::from_built_capability(&spec, ["MyCapability"])?
        .worker_child(
            lean_rs_worker::LeanWorkerChild::sibling("my_app_lean_worker")
                .env_override("MY_APP_LEAN_WORKER"),
        );

let report = builder.check();
if let Some(first) = report.first_error() {
    return Err(format!("worker bootstrap check failed: {}", first.message()).into());
}

let mut capability = builder.open()?;
```

The builder consumes the same manifest-backed descriptor as `LeanCapability`. It checks the worker child, capability
artifact, protocol handshake, import session, and optional metadata expectation without exposing child pids, pipes,
protocol frames, or loader environment variables. Typed commands, rows, diagnostics, timeout, and cycling remain behind
the worker API.

## Publishing Checklist

Keep the Lean source and Lake metadata in the published crate:

```toml
include = [
  "Cargo.toml",
  "build.rs",
  "src/**/*.rs",
  "lean/**/*.lean",
  "lean/lakefile.lean",
  "lean/lean-toolchain",
  "lean/lake-manifest.json",
]
```

Do not include `.lake/`; each downstream build should materialize Lake outputs for its local platform and Lean
toolchain.

## Gotchas

- Use `env!("LEAN_RS_CAPABILITY_MY_CAPABILITY_MANIFEST")`, not a runtime environment lookup, when the manifest is
  emitted by your own `build.rs`.
- Include Lean sources, `lakefile.lean`, `lean-toolchain`, and `lake-manifest.json` in the published crate.
- Exclude `.lake/`; it is platform- and toolchain-specific build output.
- Worker applications should ship an app-owned worker child binary that calls `lean_rs_worker::run_worker_child_stdio`.
- Do not rely on dependency binaries for the worker child; normal Cargo packaging does not install them as part of your
  application.
- Avoid nullary unboxed scalar exports in examples and app APIs. Give exported functions at least one argument, as in
  `my_app_add`, so Lake emits a function symbol rather than a persistent global.

The checked-in template under `templates/shipped-lean-crate/` shows the full layout.
