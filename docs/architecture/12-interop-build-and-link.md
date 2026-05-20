# Interop Build And Link

`lean-toolchain` owns the build-script path for downstream crates that compile
Lake shared-library targets. Callers name the Lake project root and target; they
do not construct `.lake/build/lib` paths or reimplement Lake's package-name
mangling.

## Build Script Contract

Use the checked link helper when a downstream `build.rs` wants typed errors:

```rust
use std::path::Path;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    lean_toolchain::emit_lean_link_directives_checked()?;
    let dylib = lean_toolchain::build_lake_target(Path::new("lean"), "MyCapability")?;
    println!("cargo:rustc-env=MY_CAPABILITY_DYLIB={}", dylib.display());
    Ok(())
}
```

`emit_lean_link_directives_checked` emits the Lean link-search, link-lib, rpath,
and rerun directives. If discovery fails, it returns `LinkDiagnostics` instead
of degrading the failure to a Cargo warning. `emit_lean_link_directives` remains
for callers that prefer the earlier warning-on-failure behavior.

`build_lake_target` emits `cargo:rerun-if-changed` lines for the Lake files and
Lean source files it scans. It captures Lake stdout and stderr, so stdout stays
limited to Cargo directives and caller-chosen `cargo:` lines.

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
- maximum mtime of `lakefile.lean`, `lakefile.toml`, `lean-toolchain`, and every
  `*.lean` file below the project root, excluding `.lake/`.

A hit is accepted only when the cache key matches and the expected dylib exists.
Cache write, manifest, traversal, and output-resolution failures are
`LinkDiagnostics::LakeOutputUnresolved`.

## Diagnostics

The helper path reports typed failures:

- `MissingLean`, `MissingHeader`, `MissingLib`, `UnsupportedToolchain`, and
  `VersionMismatch` from Lean toolchain discovery.
- `LakeUnavailable` when the `lake` executable cannot be started.
- `LakeTargetMissing` when the target is not declared as a `lean_lib`.
- `LakeBuildFailed` when `lake build <target>:shared` exits unsuccessfully.
- `LakeOutputUnresolved` when the manifest, cache, source scan, or expected
  dylib path cannot be resolved.

Each diagnostic renders as one line, so callers may print it through
`cargo:warning=` or return it from `build.rs`.

## Generic Interop Example

[`crates/lean-rs/examples/interop_callback.rs`](../../crates/lean-rs/examples/interop_callback.rs)
builds two Lake targets through the helper:

- `lake/lean-rs-interop-shims` target `LeanRsInterop`;
- `fixtures/interop-shims` target `LeanRsInteropConsumer`.

The example opens the generic shim dylib globally, opens the downstream-style
consumer dylib, calls an ordinary Lean export from Rust, and invokes a Lean
callback loop through `LeanCallbackHandle`. It uses no `lean-rs-host` session
API.

Run it twice to see the cache miss and cache hit paths:

```sh
cargo run -p lean-rs --example interop_callback
cargo run -p lean-rs --example interop_callback
```

The full consumer recipe is
[`docs/recipes/downstream-interop.md`](../recipes/downstream-interop.md).

## Scope

The helper builds Lake `lean_lib` shared facets for the host platform. It does
not cross-compile, build executables, publish Lake packages, or bundle shim
sources. Packaging the generic and host shim packages is a separate release
contract.
