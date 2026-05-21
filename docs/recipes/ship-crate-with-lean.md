# Ship A Crate With Lean Code

Use this recipe when a Rust crate owns Lean source code and should build on
another developer's machine with ordinary Cargo commands.

The canonical shape is build-time first:

1. `build.rs` builds the Lake shared-library target.
2. Rust opens the built dylib through `lean-rs`, or starts it behind
   `lean-rs-worker`.
3. Worker applications ship their own tiny worker-child binary.

The crate author names the Lake root, package, module, imports, and worker
child binary. The helper APIs hide Lean link directives, Lake output paths,
dylib env-var plumbing, worker protocol frames, pipes, and child startup
sequencing.

## Build The Lean Capability

Add `lean-toolchain` as a build dependency:

```toml
[build-dependencies]
lean-toolchain = "0.1"
```

Use the high-level helper in `build.rs`:

```rust,ignore
fn main() -> Result<(), Box<dyn std::error::Error>> {
    lean_toolchain::CargoLeanCapability::new("lean", "MyCapability")
        .package("my_app")
        .module("MyCapability")
        .build()?;
    Ok(())
}
```

This emits Lean link directives, Cargo rerun triggers, runs
`lake build MyCapability:shared`, resolves Lake's supported dylib naming
conventions, and emits:

```text
cargo:rustc-env=LEAN_RS_CAPABILITY_MY_CAPABILITY_DYLIB=<built dylib path>
```

Use `build_lake_target` directly only when a crate really needs lower-level
control over the build-script policy.

## Open The Capability In Process

For trusted same-process interop, embed the compile-time path with `env!`:

```rust,ignore
let runtime = lean_rs::LeanRuntime::init()?;
let capability = lean_rs::LeanCapability::from_build_env(
    runtime,
    lean_rs::LeanBuiltCapability::path(env!("LEAN_RS_CAPABILITY_MY_CAPABILITY_DYLIB"))
        .env_var("LEAN_RS_CAPABILITY_MY_CAPABILITY_DYLIB")
        .package("my_app")
        .module("MyCapability"),
)?;

let module = capability.module()?;
let answer = module.exported::<(), u64>("my_app_answer")?.call()?;
```

`LeanCapability` is a convenience layer over `LeanLibrary`: it resolves the
build-time path, opens the dylib, initializes the configured module, and keeps
the initializer names with the opened library.

## Run The Capability In A Worker

Worker applications should ship an app-owned child binary. Do not rely on a
dependency binary being installed automatically.

```rust,ignore
// src/bin/my_app_lean_worker.rs
fn main() -> std::process::ExitCode {
    lean_rs_worker::run_worker_child_stdio()
}
```

Then point the worker builder at that binary:

```rust,ignore
let spec = lean_rs::LeanBuiltCapability::path(env!("LEAN_RS_CAPABILITY_MY_CAPABILITY_DYLIB"))
    .env_var("LEAN_RS_CAPABILITY_MY_CAPABILITY_DYLIB")
    .package("my_app")
    .module("MyCapability");
let mut capability = lean_rs_worker::LeanWorkerCapabilityBuilder::from_built_capability(&spec, ["MyCapability"])?
.worker_child(
    lean_rs_worker::LeanWorkerChild::sibling("my_app_lean_worker")
        .env_override("MY_APP_LEAN_WORKER"),
)
.open()?;
```

The builder uses the built dylib path, infers the Lake root from the standard
`.lake/build/lib` layout, starts the worker child, opens the import session,
and leaves typed commands, rows, diagnostics, timeout, and cycling behind the
worker API.

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

Do not include `.lake/`; each downstream build should materialize Lake outputs
for its local platform and Lean toolchain.

The checked-in template under `templates/shipped-lean-crate/` shows the full
layout.
