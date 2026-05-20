# lean-toolchain

Lean 4 toolchain discovery, fingerprinting, link diagnostics, and `build.rs` helpers for the
`lean-rs` project. Sits above [`lean-rs-sys`](https://docs.rs/lean-rs-sys) (raw FFI + header
digest + symbol allowlist) and below [`lean-rs`](https://docs.rs/lean-rs).

Owns the typed `ToolchainFingerprint`, the Lake fixture digest, layered link diagnostics, and
reusable build-script helpers downstream embedders can call from their own `build.rs`.
Re-exports `LEAN_VERSION`, `LEAN_HEADER_DIGEST`, and `REQUIRED_SYMBOLS` from `lean-rs-sys` so
the allowlist lives in one place.

Most application code never depends on this crate directly—it shows up transitively through
`lean-rs`. Pull it in if your own `build.rs` needs Lean discovery, fingerprint, or link
diagnostics without depending on `lean-rs-sys` itself.

## Quick start in a downstream `build.rs`

```toml
[build-dependencies]
lean-toolchain = "0.1"
```

```rust,ignore
use std::path::Path;

fn main() {
    lean_toolchain::emit_lean_link_directives_checked()?;
    let dylib = lean_toolchain::build_lake_target(Path::new("lean"), "MyCapability")?;
    println!("cargo:rustc-env=MY_CAPABILITY_DYLIB={}", dylib.display());
    Ok::<(), Box<dyn std::error::Error>>(())
}
```

`build_lake_target` also covers Lake targets that depend on the generic
`lean-rs-interop-shims` package. It reports cache hits and misses on stderr,
emits only `cargo:` directives on stdout, and returns typed `LinkDiagnostics`
for missing `lake`, target lookup failures, Lake build failures, and unresolved
outputs.

See the [workspace README](https://github.com/jcreinhold/lean-rs) for the five-crate
architecture overview and [`docs/architecture/02-versioning-and-compatibility.md`](https://github.com/jcreinhold/lean-rs/blob/main/docs/architecture/02-versioning-and-compatibility.md)
for the supported Lean window.

## License

Dual-licensed under MIT or Apache-2.0 at your option.
