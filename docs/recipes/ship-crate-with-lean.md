# Ship a crate with Lean code

Use a Cargo build script to build crate-owned Lean source, then open its artifact manifest in process or in an app-owned
worker child. The template is [`templates/shipped-lean-crate/`](https://github.com/jcreinhold/lean-rs/tree/main/templates/shipped-lean-crate).

## Template and dependencies

From the repository root:

```sh
cargo run --manifest-path templates/shipped-lean-crate/Cargo.toml
cargo build --manifest-path templates/shipped-lean-crate/Cargo.toml --bin shipped-lean-crate-worker
cargo run --manifest-path templates/shipped-lean-crate/Cargo.toml --example worker
```

The current template's build script requires a configured shared-writer helper through
`LEAN_HOST_MCP_WRITER_EXECUTABLE`; its build script does not select the package-local bootstrap shown below. The first
command calls Lean in process; the other two build and run the app-owned worker child.

The template uses repository path dependencies. A published consumer uses matching released versions:

```toml
[dependencies]
lean-rs = "0.9"
lean-rs-worker-parent = "0.9" # for worker applications
lean-rs-worker-child = "0.9"  # for the app-owned child binary

[build-dependencies]
lean-toolchain = "0.9"
```

The snippets below describe the current checkout's build and loader APIs. Keep parent, child, and capability artifacts
on compatible versions when packaging them together.

## Build the capability

`CargoLeanCapability` names the Lake root, target, package, module, and trusted export ABI. For Lean sources and all
materialized dependencies strictly below one Cargo package, an explicit `PrivateLakeBootstrap` permits package-local
builds before a shared-writer executable exists:

```rust,ignore
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use lean_toolchain::{
        LeanExportAbiRepr, LeanExportArgAbi, LeanExportOwnership, LeanExportResultConvention, LeanExportReturnAbi,
        LeanExportSignature,
    };

    let owner = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR")?);
    let lean_root = owner.join("lean");
    let bootstrap = lean_toolchain::shared_writer::PrivateLakeBootstrap::new(&owner, &lean_root)?;
    lean_toolchain::CargoLeanCapability::new(&lean_root, "MyCapability")
        .private_bootstrap(bootstrap)
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

For shared project roots or dependencies outside the Cargo owner, use the configured shared-writer helper instead;
`private_bootstrap` rejects escaped roots. The helper emits Lean link directives and Cargo rerun triggers, builds or
reuses the Lake shared target, and writes a JSON capability manifest. Its compile-time outputs are:

```text
cargo:rustc-env=LEAN_RS_CAPABILITY_MY_CAPABILITY_MANIFEST=<manifest path>
cargo:rustc-env=LEAN_RS_CAPABILITY_MY_CAPABILITY_DYLIB=<built dylib path>
```

Use the manifest as the runtime handoff. The dylib output supports low-level compatibility callers. The export metadata
is supplied by the crate author; it must match the compiled Lean export. Checked lookup compares Rust's call shape with
that trusted metadata.

## Open in process

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

`LeanCapability` loads the primary library and dependency bundle, initializes the module, and retains its trusted export
signatures. A missing or mismatched signature fails before dispatch. `LeanCapabilityPreflight` checks artifacts,
fingerprints, manifests, and initializer symbols for installer or doctor commands; the report uses stable loader codes
and repair hints.

Use a compile-time `env!` for paths emitted by this crate's build script. These are local build-artifact paths;
relocating a prebuilt application requires packaging its libraries and a descriptor that resolves their installed
locations.

## Open in a worker

Cargo does not install a dependency's binary for an application. Ship a child binary of your own:

```rust,ignore
// src/bin/my_app_lean_worker.rs
fn main() -> std::process::ExitCode {
    lean_rs_worker_child::run_worker_child_stdio()
}
```

Point the builder at that binary and the same manifest:

```rust,ignore
let spec = lean_rs::LeanBuiltCapability::manifest_path(
    env!("LEAN_RS_CAPABILITY_MY_CAPABILITY_MANIFEST"),
)
.manifest_env_var("LEAN_RS_CAPABILITY_MY_CAPABILITY_MANIFEST");
let builder = lean_rs_worker_parent::LeanWorkerCapabilityBuilder::from_built_capability(
    &spec,
    ["MyCapability"],
)?
.worker_child(
    lean_rs_worker_parent::LeanWorkerChild::sibling("my_app_lean_worker")
        .env_override("MY_APP_LEAN_WORKER"),
);
let report = builder.check();
if let Some(first) = report.first_error() {
    return Err(format!("worker bootstrap check failed: {}", first.message()).into());
}
let mut capability = builder.open()?;
```

The active bootstrap probe checks the child, artifacts, handshake, import session, and any metadata expectation. See the
[worker runner](worker-capability-runner.md) for command signatures, typed rows, diagnostics, and lifecycle behavior.

## Publish the source package

Include the Rust build script and Lean package inputs:

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

Exclude `.lake/`; downstream builds need outputs for their own platform and Lean toolchain. If the package includes
examples, include their source files too. Give scalar exports at least one argument: a nullary unboxed scalar may be
emitted as a persistent global rather than a callable function symbol.
