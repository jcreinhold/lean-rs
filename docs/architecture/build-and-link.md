# Interop Build And Link

`lean-toolchain` owns the build-script path for downstream crates that compile Lake shared-library targets. Callers name
the Lake project root and target, and Lake names every path. Neither callers nor `lean-toolchain` construct
`.lake/build/lib` paths, reimplement Lake's package-name mangling, or read `lake-manifest.json`.

## Build Script Contract

A build script names the Lake project root and target:

```rust
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let lean_root = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR")?).join("lean");
    lean_toolchain::CargoLeanCapability::new(&lean_root, "MyCapability")
        .package("my_app")
        .module("MyCapability")
        .build()?;
    Ok(())
}
```

The helper emits the Lean link-search, link-lib, runtime rpath, Cargo rerun directives, a deterministic manifest
compile-time environment variable, and a compatibility dylib-path variable:

```text
cargo:rustc-env=LEAN_RS_CAPABILITY_MY_CAPABILITY_MANIFEST=<manifest path>
cargo:rustc-env=LEAN_RS_CAPABILITY_MY_CAPABILITY_DYLIB=<built dylib path>
```

It runs `lake query MyCapability:shared`, which builds the library when Lake finds it out of date and prints its path. It
then writes a self-describing artifact manifest beside that library, or into `OUT_DIR` when a build script builds
without a generated-output key, and returns `BuiltLeanCapability` for build scripts that want to inspect the result. The
manifest is the canonical runtime handoff for shipped crates; the direct dylib path remains a lower-level compatibility
path.

The helper also asks Lake for the `LeanRsInterop` library. When the workspace has one, for example through a `require`
of `lean-rs-interop-shims`, the manifest records it as the first dependency. Record other separately built dependency
libraries explicitly. Hosts selecting their own Lean toolchain can also pass the matching sysroot:

```rust,ignore
use lean_toolchain::LeanLibraryDependency;

lean_toolchain::CargoLeanCapability::new("lean", "MyCapability")
    .package("my_app")
    .module("MyCapability")
    .dependency(
        LeanLibraryDependency::path(dependency_dylib)
            .export_symbols_for_dependents()
            .initializer("dependency_pkg", "Dependency"),
    )
    .lean_sysroot(consumer_lean_sysroot)
    .build()?;
```

The dependency and explicit sysroot are optional. Dependency metadata tells the loader what to open; the owning crate
still decides how sources are packaged, the Lake package/module names, and which exports are trusted.

The selected sysroot is scoped to this helper call. Link-directive discovery uses that prefix, and the Lake build runs
`<sysroot>/bin/lake query <target>:shared` with `LEAN_SYSROOT` set only on the child command. The helper does not set
process-global environment variables or expose a raw `LEAN_PATH` override.

Lower-level helpers remain available for custom build flows:

- `emit_lean_link_directives_checked` emits only Lean link directives and returns typed `LinkDiagnostics` on discovery
  failure.
- `emit_lean_link_directives` preserves the warning-on-failure behavior.
- `build_lake_target` builds one Lake shared-library target and emits Cargo rerun directives.
- `LakeWorkspace` runs the same Lake commands without Cargo directives, for runtime code and tests: `lean_path` runs
  `lake env`, `shared_library` runs `lake query`, and `built_shared_library` runs `lake --no-build query`. The host's
  bundled shim path uses `CargoLeanCapability::build_quiet` so its manifest also carries trusted export signatures.

`build_lake_target` emits `cargo:rerun-if-changed` for each regular file at the top of the project and each `.lean` file
below it, skipping directories whose names start with `.`. It captures Lake stdout and stderr, so stdout stays limited
to Cargo directives and caller-chosen `cargo:` lines.

## What Lake Answers

`lean-toolchain` keeps no build cache of its own. Lake decides whether a library is out of date and where its files go:

- `lake query <target>:shared` builds the library and the libraries it imports when they are out of date, then prints
  the shared file's path. Lake takes the file name from the library's `libName` when the lakefile sets one.
- `lake --no-build query <target>:shared` exits with code 3 when anything would have to be built. `probe_ready()` and
  `LakeWorkspace::built_shared_library` report that as `None`.
- `lake env` prints `LEAN_PATH`: each package's `.olean` directory, dependencies first and the root last. Lake ends the
  list with its own library directory, which `LakeWorkspace::lean_path` drops.
- Lake's `unknown target` error means the workspace has no library by that name; the helpers report it as
  `LakeTargetMissing`.

Every Lake command loads the workspace first, so even a probe may write Lake's own files: its configuration cache, a
missing `lake-manifest.json`, or a dependency checkout.

`generated_output_key(key)` serves the bundled shim packages, which build into `.lake/lean-rs-generated/lean-4.36.0`
under Lean 4.36.0. The host shim package picks that directory by version; the interop package picks it only when the
`leanRsGeneratedShims` option is `true`, which the host package and every keyed build pass. Lake reuses a cached
configuration without checking which options built it. So when a keyed build finds a library outside the keyed
directory, it has Lake read its lakefiles again (`-R`) once, and fails if the library is still outside. A probe never
does this. With a key, the helper also refuses a library, or a probed manifest, that it reaches through a symlink or
that has a second hard link.

## Concurrent Builds

`CargoLeanCapability` runs Lake directly and takes no lock on the project, and Lake takes no build lock either. Cargo
serializes builds that share a target directory, but nothing stops two processes from building one Lake project at once,
or from rebuilding it while a worker reads its outputs. The process that owns the checkout orders its builds and reads, for example one server per
checkout with an in-memory readers–writer turn.

`probe_ready()` runs `lake --no-build query` for the library, and for `LeanRsInterop` when the workspace has it, then
checks the exact expected capability manifest, including ABI exports, selected fingerprint and dependencies. It builds
nothing, though Lake may write its own files. A stale library, a missing dependency file or a different manifest returns
`None`; each separately prepared dependency needs its own probe. `with_materialized_source_package` holds an exclusive lock on one source-cache entry
while it installs the payload and runs the caller's build, because several build scripts can share one cache root.

## Diagnostics

The helper path reports typed failures:

- `MissingLean`, `MissingHeader`, `MissingLib`, `UnsupportedToolchain`, and `VersionMismatch` from Lean toolchain
  discovery.
- `LakeConfigMissing` when the project root has no `lakefile.lean` or `lakefile.toml`.
- `LakeTargetMissing` when Lake reports no library by that name.
- `LakeUnavailable` when the `lake` executable cannot be started.
- `LakeFailed` when a Lake command exits unsuccessfully.
- `LakeAnswerInvalid` when Lake succeeds but prints something other than the answer it documents.
- `LakeOutputUnresolved` when the helper refuses the capability's settings or Lake's answer, or cannot write the
  manifest.

Each diagnostic renders as one line, so callers may print it through `cargo:warning=` or return it from `build.rs`.

## Generic Interop Example

[`crates/lean-rs/examples/interop_callback.rs`](https://github.com/jcreinhold/lean-rs/blob/main/crates/lean-rs/examples/interop_callback.rs)
builds two Lake targets through the helper:

- `crates/lean-rs/shims/lean-rs-interop-shims` target `LeanRsInterop`;
- `fixtures/interop-shims` target `LeanRsInteropConsumer`.

The example opens the generic shim dylib globally, opens the downstream-style consumer dylib, calls an ordinary Lean
export from Rust, and invokes a Lean callback loop through `LeanCallbackHandle`. It uses no `lean-rs-host` session API.

Run the example twice; on the second run Lake finds both libraries up to date and builds nothing:

```sh
cargo run -p lean-rs --example interop_callback
cargo run -p lean-rs --example interop_callback
```

The canonical shipped-crate recipe is [`docs/recipes/ship-crate-with-lean.md`](../recipes/ship-crate-with-lean.md). The
advanced same-process callback recipe is [`docs/recipes/downstream-interop.md`](../recipes/downstream-interop.md).

## Scope

The helper builds Lake `lean_lib` shared facets for the host platform. It does not cross-compile, build executables, or
publish Lake packages. The generic and host shim sources are bundled with their owning Rust crates; this helper is the
path those crates and downstream examples use to materialize the dylibs.
