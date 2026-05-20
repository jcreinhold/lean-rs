# lean-rs

Safe Rust bindings for hosting Lean 4 capabilities. The single safe front door of the
`lean-rs` project: runtime initialization, owned and borrowed object handles, typed
first-order ABI conversions, module loading and exported functions, semantic handles
(`LeanName`, `LeanLevel`, `LeanExpr`, `LeanDeclaration`), and a structured error/diagnostic
boundary.

Ships **zero Lean-side code**. This is the minimum every mainstream Rust binding to a
GC-hosted language follows (`ocaml-rs`, `hs-bindgen`, `caml-oxide`). If you want an
opinionated theorem-prover-host stack with sessions, `MetaM`, and kernel-checked evidence,
add [`lean-rs-host`](https://docs.rs/lean-rs-host) on top.

## Quick start

The minimum L1 consumer is five files: a Lean module declaring an `@[export]`, a `lakefile.lean`
that builds it into a shared library, a Rust `build.rs` that emits the link and rpath
directives, a `Cargo.toml`, and a Rust caller. All five together fit on one screen.

**`Cargo.toml`**:

```toml
[package]
name = "my_app"
version = "0.1.0"
edition = "2024"

[dependencies]
lean-rs = "0.1"

[build-dependencies]
lean-toolchain = "0.1"
```

**`build.rs`**—one helper covers link-search, link-lib, and the runtime rpath into the Lean
toolchain's `lib/lean` directory; the other builds the Lake shared-library target and records
the dylib path for `main.rs`:

```rust
use std::path::Path;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    lean_toolchain::emit_lean_link_directives_checked()?;
    let dylib = lean_toolchain::build_lake_target(Path::new("lean"), "MyCapability")?;
    println!("cargo:rustc-env=MY_CAPABILITY_DYLIB={}", dylib.display());
    Ok(())
}
```

**`lean/lakefile.lean`**—a minimal Lake package emitting one shared library:

```lean
import Lake
open Lake DSL

package «my_app»

@[default_target]
lean_lib «MyCapability» where
  defaultFacets := #[LeanLib.sharedFacet]
```

**`lean/MyCapability.lean`**—one Rust-callable export:

```lean
@[export my_app_add]
def add (a b : UInt64) : UInt64 := a + b
```

**`src/main.rs`**—open the dylib, dispatch typed:

```rust
use lean_rs::{LeanLibrary, LeanResult, LeanRuntime};

fn main() -> LeanResult<()> {
    let runtime = LeanRuntime::init()?;
    let library = LeanLibrary::open(runtime, env!("MY_CAPABILITY_DYLIB"))?;
    let module = library.initialize_module("my_app", "MyCapability")?;

    let add = module.exported::<(u64, u64), u64>("my_app_add")?;
    println!("{}", add.call((40, 2))?);  // 42
    Ok(())
}
```

Build and run:

```sh
cargo run
```

`build_lake_target` hides Lake's shared-library facet, cache, and filename convention.
`initialize_module(package, lib_name)` still takes unmangled names; the loader resolves the
mangled initializer symbol internally.

For a complete downstream-style example that also lets Lean call a Rust callback, run
`cargo run -p lean-rs --example interop_callback` in the workspace and read
[`docs/recipes/downstream-interop.md`](https://github.com/jcreinhold/lean-rs/blob/main/docs/recipes/downstream-interop.md).

Pre-publish (`lean-rs 0.1.0` is not yet on crates.io), pin against the workspace by adding
`path = "../path/to/lean-rs/crates/lean-rs"` alongside `version = "0.1"` in `Cargo.toml`.
Same for `lean-toolchain`.

The `Args` and `R` generics on `LeanModule::exported` are sealed by the `LeanAbi` /
`LeanArgs` / `DecodeCallResult` traits, so unsupported types fail at compile time rather than
producing wrong decodes at runtime.

## See also

- Workspace overview, architecture docs, and the worked examples that exercise this surface end to end: [`lean-rs` repository](https://github.com/jcreinhold/lean-rs).
- L2 host stack (sessions, kernel check, `MetaM`): [`lean-rs-host`](https://docs.rs/lean-rs-host).
- Build-script helper: [`lean-toolchain`](https://docs.rs/lean-toolchain).
- Raw FFI escape hatch (advanced): [`lean-rs-sys`](https://docs.rs/lean-rs-sys).
- Diagnostics and tracing: [`docs/diagnostics.md`](https://github.com/jcreinhold/lean-rs/blob/main/docs/diagnostics.md).

## License

Dual-licensed under MIT or Apache-2.0 at your option.
