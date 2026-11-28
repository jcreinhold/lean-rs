# lean-toolchain

Lean 4 toolchain discovery, fingerprinting, link diagnostics, and `build.rs` helpers for the `lean-rs` project. Sits
above [`lean-rs-abi`](https://docs.rs/lean-rs-abi) (link-free ABI/toolchain metadata) and below
[`lean-rs`](https://docs.rs/lean-rs).

Owns the typed `ToolchainFingerprint`, layered link diagnostics, and reusable build-script helpers downstream embedders
can call from their own `build.rs`. Its `build.rs` probes the active toolchain (degrading to the latest supported entry
when none is installed) to bake the live `LEAN_VERSION`, `LEAN_HEADER_PATH`, `LEAN_HEADER_DIGEST`, and
`LEAN_RESOLVED_VERSION`; it re-exports `REQUIRED_SYMBOLS` and the supported-window table from the purely static
`lean-rs-abi` so metadata consumers depend on neither the raw FFI/link crate nor a probe in `abi`.

`LakeWorkspace` asks Lake for a workspace's import roots (`lake env`, dependencies first and the root last) and for a
library's shared file (`lake query <lib>:shared`, with or without building it). A caller that runs processes under its
own time limit or process group can take each Lake command from `LakeWorkspace` and hand back the output for it to read.
Lake owns the package graph and the output layout; this crate reads no `lake-manifest.json` and computes no Lake output
path.

Runtime application code usually does not depend on this crate directly: it shows up transitively through `lean-rs`.
Downstream crates that ship Lean source commonly depend on it from `build.rs`, where `CargoLeanCapability` builds the
Lake shared library and emits the compile-time path that runtime code opens through `lean-rs` or
`lean-rs-worker-parent`. Same-process apps pair that path with `LeanCapability`; worker apps pair it with
`LeanWorkerChild` and an app-owned worker-child binary.

`CargoLeanCapability` runs Lake directly and takes no lock. Cargo serializes builds that share a target directory; a
process that also reads the outputs, such as a server, orders its own builds and reads. See the architecture's
Concurrent Builds section. `process_group::OwnedCommandGroup` runs a command in its own process group and keeps it until
the group has gone; on macOS it waits for the whole group, while other Unix backends only observe its absence.

See
[`docs/recipes/ship-crate-with-lean.md`](https://github.com/jcreinhold/lean-rs/blob/main/docs/recipes/ship-crate-with-lean.md).

## Quick start in a downstream `build.rs`

```toml
[build-dependencies]
lean-toolchain = "0.12"
```

```rust,ignore
fn main() -> Result<(), Box<dyn std::error::Error>> {
    lean_toolchain::CargoLeanCapability::new("lean", "MyCapability")
        .package("my_app")
        .module("MyCapability")
        // Optional: pin the Lean prefix used for link discovery and the Lake child process.
        // .lean_sysroot(consumer_lean_sysroot)
        .build()?;
    Ok(())
}
```

`CargoLeanCapability` emits Lean link directives and Cargo rerun triggers, runs `lake query <target>:shared`, which
builds the library when it is out of date and prints its path, and writes a JSON artifact manifest. It also asks Lake
for the `LeanRsInterop` library; when the workspace has one, the manifest lists it as the first dependency. The manifest
goes beside the library, or into `OUT_DIR` when a build script builds without a generated-output key. The helper emits
deterministic `LEAN_RS_CAPABILITY_<TARGET>_MANIFEST` plus compatibility `LEAN_RS_CAPABILITY_<TARGET>_DYLIB` compile-time
environment variables. `probe_ready` runs `lake --no-build query` instead and returns `None` when Lake would rebuild the
library or the manifest differs. The helper prints only `cargo:` directives on stdout and returns typed
`LinkDiagnostics` for a missing lakefile, a library Lake does not know, a `lake` that cannot start or fails, and an
answer it cannot use.

Use `.lean_sysroot(...)` when a host must build the capability with the same Lean prefix its worker child will use. The
helper runs that prefix's `bin/lake` with a child-scoped `LEAN_SYSROOT`; it does not mutate the parent process
environment.

See
[`docs/recipes/ship-crate-with-lean.md`](https://github.com/jcreinhold/lean-rs/blob/main/docs/recipes/ship-crate-with-lean.md)
for the canonical shipped-crate layout.

See the [workspace README](https://github.com/jcreinhold/lean-rs) for the workspace architecture overview and
[`docs/architecture/compatibility.md`](https://github.com/jcreinhold/lean-rs/blob/main/docs/architecture/compatibility.md)
for the supported Lean window.

## License

Dual-licensed under MIT or Apache-2.0 at your option.
