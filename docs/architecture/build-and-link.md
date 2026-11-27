# Interop Build And Link

`lean-toolchain` owns the build-script path for downstream crates that compile Lake shared-library targets. Callers name
the Lake project root and target; they do not construct `.lake/build/lib` paths or reimplement Lake's package-name
mangling.

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

It runs `lake build MyCapability:shared`, resolves Lake's supported dylib naming conventions, writes a self-describing
artifact manifest, and returns `BuiltLeanCapability` for build scripts that want to inspect the result. The manifest is
the canonical runtime handoff for shipped crates; the direct dylib path remains a lower-level compatibility path.

Record separately built dependency libraries in the artifact manifest. Hosts selecting their own Lean toolchain can also
pass the matching sysroot:

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
`<sysroot>/bin/lake build <target>:shared` with `LEAN_SYSROOT` set only on the child command. The helper does not set
process-global environment variables or expose a raw `LEAN_PATH` override.

Lower-level helpers remain available for custom build flows:

- `emit_lean_link_directives_checked` emits only Lean link directives and returns typed `LinkDiagnostics` on discovery
  failure.
- `emit_lean_link_directives` preserves the warning-on-failure behavior.
- `build_lake_target` builds one Lake shared-library target and emits Cargo rerun directives.
- `build_lake_target_quiet` uses the same cache and path resolver without emitting Cargo directives. The host's bundled
  shim path uses `CargoLeanCapability::build_quiet` so its manifest also carries trusted export signatures.

`build_lake_target` emits `cargo:rerun-if-changed` lines for the Lake files and Lean source files it scans. It captures
Lake stdout and stderr, so stdout stays limited to Cargo directives and caller-chosen `cargo:` lines.

Human cache diagnostics go to stderr:

```text
lean-toolchain: cache hit for Lake target `LeanRsInterop` in ...; using ...
lean-toolchain: cache miss for Lake target `LeanRsInterop` in ...; running `lake build LeanRsInterop:shared`
```

## Cache Key

The version 2 cache key covers the Lake project and its bounded declared dependency closure:

- SHA-256 of `lake-manifest.json`;
- target name and Lake package name;
- explicit Lean sysroot path, or the ambient-toolchain selector;
- count of scanned source files;
- maximum mtime of `lakefile.lean`, `lakefile.toml`, `lean-toolchain`, and every `*.lean` file below the project root,
  excluding `.lake/`;
- streaming content SHA-256 of the primary project and each canonical dependency's supported owned file tree, excluding
  `.lake` and `.git`, including C/header/resource inputs, Lake configuration, toolchain pin, manifest and supported
  override metadata; this detects content changes even when size and modification timestamps are preserved;
- file and directory identity observations, including Unix device, inode and change time, which also detect ordinary
  restored edits and transient source creation/removal; non-Unix platforms lack that Unix change-time evidence;
- content SHA-256 of dependent dylibs recorded by the capability builder, excluding the target's own output.

Before cache publication the complete source input fingerprint must still equal the pre-build fingerprint. A changed
input set refuses publication rather than stamping new source identity onto an artifact built from older inputs. Each
streamed file and traversed directory must also retain its observation while the fingerprint is taken. This is bounded
workspace observation, not an immutable filesystem snapshot or protection against arbitrary external I/O. The build
creates each package's `.lake` directory before its initial source observation. Lake may rebuild dependency outputs
during a successful command; their final content trace is saved for later cache checks. Earlier cache versions miss once
and rebuild.

A hit is accepted only when the cache key matches and the expected dylib exists. Cache write, manifest, traversal, and
output-resolution failures are `LinkDiagnostics::LakeOutputUnresolved`.

After the target succeeds, `CargoLeanCapability` checks each dependency library before publishing the capability
manifest. A separately built dependency that is still missing yields `LakeOutputUnresolved` with its path, and
`probe_ready()` reports a cache miss for it. Build the missing dependency, then refresh the capability.

## Concurrent Builds

`CargoLeanCapability` runs Lake directly and takes no lock on the project. Cargo serializes builds that share a target
directory, but nothing in the SDK stops two processes from building one Lake project at once, or from rebuilding it while
a worker reads its outputs. The process that owns the checkout orders its builds and reads, for example one server per
checkout with an in-memory readers–writer turn.

`probe_ready()` checks the build-cache key and the exact expected capability manifest, including ABI exports, selected
fingerprint and dependencies, without running Lake or writing anything. A miss returns `None`; each separately prepared
dependency needs its own probe. `with_materialized_source_package` holds an exclusive lock on one source-cache entry
while it installs the payload and runs the caller's build, because several build scripts can share one cache root.

## Diagnostics

The helper path reports typed failures:

- `MissingLean`, `MissingHeader`, `MissingLib`, `UnsupportedToolchain`, and `VersionMismatch` from Lean toolchain
  discovery.
- `LakeUnavailable` when the `lake` executable cannot be started.
- `LakeTargetMissing` when the target is not declared as a `lean_lib`.
- `LakeBuildFailed` when `lake build <target>:shared` exits unsuccessfully.
- `LakeOutputUnresolved` when the manifest, cache, source scan, or expected dylib path cannot be resolved.

Each diagnostic renders as one line, so callers may print it through `cargo:warning=` or return it from `build.rs`.

## Generic Interop Example

[`crates/lean-rs/examples/interop_callback.rs`](https://github.com/jcreinhold/lean-rs/blob/main/crates/lean-rs/examples/interop_callback.rs)
builds two Lake targets through the helper:

- `crates/lean-rs/shims/lean-rs-interop-shims` target `LeanRsInterop`;
- `fixtures/interop-shims` target `LeanRsInteropConsumer`.

The example opens the generic shim dylib globally, opens the downstream-style consumer dylib, calls an ordinary Lean
export from Rust, and invokes a Lean callback loop through `LeanCallbackHandle`. It uses no `lean-rs-host` session API.

Run the example against a cold cache, and repeat to exercise the cache-hit path:

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
