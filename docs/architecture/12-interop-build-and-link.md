# Interop Build And Link

`lean-toolchain` owns the build-script path for downstream crates that compile Lake shared-library targets. Callers name
the Lake project root and target; they do not construct `.lake/build/lib` paths or reimplement Lake's package-name
mangling.

## Build Script Contract

Use `CargoLeanCapability` for downstream crates that ship Lean source:

```rust
fn main() -> Result<(), Box<dyn std::error::Error>> {
    lean_toolchain::CargoLeanCapability::new("lean", "MyCapability")
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

Lower-level helpers remain available for custom build flows:

- `emit_lean_link_directives_checked` emits only Lean link directives and returns typed `LinkDiagnostics` on discovery
  failure.
- `emit_lean_link_directives` preserves the warning-on-failure behavior.
- `build_lake_target` builds one Lake shared-library target and emits Cargo rerun directives.
- `build_lake_target_quiet` uses the same cache and path resolver without emitting Cargo directives; `lean-rs-host` uses
  it when it materializes bundled shims at runtime.

`build_lake_target` emits `cargo:rerun-if-changed` lines for the Lake files and Lean source files it scans. It captures
Lake stdout and stderr, so stdout stays limited to Cargo directives and caller-chosen `cargo:` lines.

`discover_lake_modules` is the runtime discovery companion for higher-level planners. It resolves the Lake root,
discovers `lean_lib` module roots, enumerates source modules deterministically, and returns source-set fingerprints
without building a shared library. `lean-rs-worker` uses that general discovery output for import-set planning;
downstream crates should not reimplement Lake source-root or module-path conventions.

Human cache diagnostics go to stderr:

```text
lean-toolchain: cache hit for Lake target `LeanRsInterop` in ...; using ...
lean-toolchain: cache miss for Lake target `LeanRsInterop` in ...; running `lake build LeanRsInterop:shared`
```

## Cache Key

The cache key is local to the Lake project:

- SHA-256 of `lake-manifest.json`;
- target name and Lake package name;
- count of scanned source files;
- maximum mtime of `lakefile.lean`, `lakefile.toml`, `lean-toolchain`, and every `*.lean` file below the project root,
  excluding `.lake/`.

A hit is accepted only when the cache key matches and the expected dylib exists. Cache write, manifest, traversal, and
output-resolution failures are `LinkDiagnostics::LakeOutputUnresolved`.

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

[`crates/lean-rs/examples/interop_callback.rs`](../../crates/lean-rs/examples/interop_callback.rs) builds two Lake
targets through the helper:

- `crates/lean-rs/shims/lean-rs-interop-shims` target `LeanRsInterop`;
- `fixtures/interop-shims` target `LeanRsInteropConsumer`.

The example opens the generic shim dylib globally, opens the downstream-style consumer dylib, calls an ordinary Lean
export from Rust, and invokes a Lean callback loop through `LeanCallbackHandle`. It uses no `lean-rs-host` session API.

Run it twice to see the cache miss and cache hit paths:

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
