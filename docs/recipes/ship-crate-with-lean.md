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

The template's build script runs Lake for the packaged demo and the host-owned shims. The first command calls Lean in
process. The other two build the application-owned child and open a worker session.

The template depends on the workspace's matching `lean-rs`, `lean-rs-host`, `lean-rs-worker-parent` and
`lean-rs-worker-child` crates by path, and its build script on `lean-toolchain`. Its `Cargo.toml` also lists the dependencies for the application-owned artifact
checks. Keep the parent, child, runtime and capability artifacts compatible when packaging them together.

## Build the capability

`CargoLeanCapability` names the Lake root, target, package, module, and trusted export ABI:

```rust,ignore
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use lean_toolchain::{
        LeanExportAbiRepr, LeanExportArgAbi, LeanExportOwnership, LeanExportResultConvention, LeanExportReturnAbi,
        LeanExportSignature,
    };

    let lean_root = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR")?).join("lean");
    lean_toolchain::CargoLeanCapability::new(&lean_root, "MyCapability")
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

The builder runs Lake directly and takes no lock; see
[Concurrent builds](../architecture/build-and-link.md#concurrent-builds). It emits Lean link directives and Cargo rerun
triggers, builds or reuses the Lake shared target, and writes a JSON capability manifest. Its compile-time outputs are:

```text
cargo:rustc-env=LEAN_RS_CAPABILITY_MY_CAPABILITY_MANIFEST=<manifest path>
cargo:rustc-env=LEAN_RS_CAPABILITY_MY_CAPABILITY_DYLIB=<built dylib path>
```

Use the manifest as the runtime handoff. The dylib output supports low-level compatibility callers. The export metadata
is supplied by the crate author; it must match the compiled Lean export. Checked lookup compares Rust's call shape with
that trusted metadata.

## Open in process

The template embeds the manifest and image selected by its build, verifies the expected module and dependency list,
and writes the image bytes into an application-owned private directory. It uses the matching Lean runtime and
passes that directory's guard to `TrustedWorkerCapability::load_snapshot`, which keeps the directory until the
capability drops. The unsafe call admits exactly
`ship_lean_demo_add` as `WorkerExportContract::U64Binary`. The resulting typed binding runs in the current process.
[Complete in-process caller](https://github.com/jcreinhold/lean-rs/blob/main/templates/shipped-lean-crate/src/main.rs)

```rust,ignore
let answer = capability.bind_binary("ship_lean_demo_add")?.call(40, 2)?;
```

For generic dispatch, `LeanCapability::from_build_manifest`, `LeanCapability::exported` and
`LeanExported::call` each require the application's unsafe assertion. Preflight checks artifact metadata and symbol
presence. The caller establishes that the actual image implements the declared ABI and remains live for every use.
[Loader contract](../architecture/loading.md#native-ownership-in-010)

Use a compile-time `env!` for paths emitted by this crate's build script. These are local build-artifact paths;
relocating a prebuilt application requires packaging its libraries and a descriptor that resolves their installed
locations.

## Open in a worker

Ship an application-owned child. Its `WorkerAuthority` implementation checks the requested manifest bytes, images
and initializer against the deployment it owns and admits the symbols it serves. The template installs that
authority with `run_worker_child_with_authority`.
[Worker entrypoint](https://github.com/jcreinhold/lean-rs/blob/main/templates/shipped-lean-crate/src/bin/shipped_lean_crate_worker.rs)

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
