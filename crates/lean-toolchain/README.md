# lean-toolchain

Lean 4 toolchain discovery, fingerprinting, link diagnostics, and `build.rs` helpers for the `lean-rs` project. Sits
above [`lean-rs-abi`](https://docs.rs/lean-rs-abi) (link-free ABI/toolchain metadata) and below
[`lean-rs`](https://docs.rs/lean-rs).

Owns the typed `ToolchainFingerprint`, the Lake fixture digest, layered link diagnostics, and reusable build-script
helpers downstream embedders can call from their own `build.rs`. Its `build.rs` probes the active toolchain (degrading to
the latest supported entry when none is installed) to bake the live `LEAN_VERSION`, `LEAN_HEADER_PATH`,
`LEAN_HEADER_DIGEST`, and `LEAN_RESOLVED_VERSION`; it re-exports `REQUIRED_SYMBOLS` and the supported-window table from
the purely static `lean-rs-abi` so metadata consumers depend on neither the raw FFI/link crate nor a probe in `abi`.

It also owns Lake module discovery for higher layers. `discover_lake_modules` resolves a Lake root, discovers `lean_lib`
source roots, validates module names, enumerates Lean source files deterministically, and returns source-set
fingerprints. This discovery layer does not know worker pools or downstream cache policy; `lean-rs-worker-parent` uses
it to build import/session batches.

Runtime application code usually does not depend on this crate directly: it shows up transitively through `lean-rs`.
Downstream crates that ship Lean source commonly depend on it from `build.rs`, where `CargoLeanCapability` builds the
Lake shared library and emits the compile-time path that runtime code opens through `lean-rs` or `lean-rs-worker-parent`.
Same-process apps pair that path with `LeanCapability`; worker apps pair it with `LeanWorkerChild` and an app-owned
worker-child binary. See
[`docs/recipes/ship-crate-with-lean.md`](https://github.com/jcreinhold/lean-rs/blob/main/docs/recipes/ship-crate-with-lean.md).

## Quick start in a downstream `build.rs`

```toml
[build-dependencies]
lean-toolchain = "0.2"
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

`CargoLeanCapability` emits Lean link directives, Cargo rerun triggers, runs `lake build <target>:shared`, resolves the
built dylib path, writes a JSON artifact manifest, and emits deterministic `LEAN_RS_CAPABILITY_<TARGET>_MANIFEST` plus
compatibility `LEAN_RS_CAPABILITY_<TARGET>_DYLIB` compile-time environment variables. `build_lake_target` remains
available as the lower-level escape hatch and also covers Lake targets that depend on the generic
`lean-rs-interop-shims` package. It reports cache hits and misses on stderr, emits only `cargo:` directives on stdout,
and returns typed `LinkDiagnostics` for missing `lake`, target lookup failures, Lake build failures, and unresolved
outputs.

Use `.lean_sysroot(...)` when a host must build the capability with the same Lean prefix its worker child will use. The
helper runs that prefix's `bin/lake` with a child-scoped `LEAN_SYSROOT`; it does not mutate the parent process
environment.

See
[`docs/recipes/ship-crate-with-lean.md`](https://github.com/jcreinhold/lean-rs/blob/main/docs/recipes/ship-crate-with-lean.md)
for the canonical shipped-crate layout.

See the [workspace README](https://github.com/jcreinhold/lean-rs) for the workspace architecture overview and
[`docs/architecture/02-versioning-and-compatibility.md`](https://github.com/jcreinhold/lean-rs/blob/main/docs/architecture/02-versioning-and-compatibility.md)
for the supported Lean window.

## License

Dual-licensed under MIT or Apache-2.0 at your option.
