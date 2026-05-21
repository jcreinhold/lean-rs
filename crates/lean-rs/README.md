# lean-rs

Safe Rust bindings for hosting Lean 4 capabilities. The single safe front door of the
`lean-rs` project: runtime initialization, owned and borrowed object handles, typed
first-order ABI conversions, module loading and exported functions, semantic handles
(`LeanName`, `LeanLevel`, `LeanExpr`, `LeanDeclaration`), and a structured error/diagnostic
boundary.

Ships the generic interop shim package used by same-process Lean-to-Rust callbacks, but no
theorem-prover host shims. If you want sessions, `MetaM`, and kernel-checked evidence, add
[`lean-rs-host`](https://docs.rs/lean-rs-host) on top.

## Quick start

The minimum consumer is five pieces: a Lean module declaring an `@[export]`, a `lakefile.lean`
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

**`build.rs`**—one helper covers link-search, link-lib, the runtime rpath, Lake build,
Cargo rerun triggers, and the compile-time dylib path:

```rust
fn main() -> Result<(), Box<dyn std::error::Error>> {
    lean_toolchain::CargoLeanCapability::new("lean", "MyCapability")
        .package("my_app")
        .module("MyCapability")
        .build()?;
    Ok(())
}
```

**`lean/lakefile.lean`**: a minimal Lake package emitting one shared library. Lake uses
guillemets (`« »`) as its idiomatic quoting for package and library names; plain ASCII
names also work.

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

**`src/main.rs`**—open the build-script capability, dispatch typed:

```rust
use lean_rs::{LeanBuiltCapability, LeanCapability, LeanResult, LeanRuntime};

fn main() -> LeanResult<()> {
    let runtime = LeanRuntime::init()?;
    let capability = LeanCapability::from_build_env(
        runtime,
        LeanBuiltCapability::path(env!("LEAN_RS_CAPABILITY_MY_CAPABILITY_DYLIB"))
            .env_var("LEAN_RS_CAPABILITY_MY_CAPABILITY_DYLIB")
            .package("my_app")
            .module("MyCapability"),
    )?;
    let module = capability.module()?;

    let add = module.exported::<(u64, u64), u64>("my_app_add")?;
    println!("{}", add.call(40, 2)?);  // 42
    Ok(())
}
```

Build and run:

```sh
cargo run
```

`CargoLeanCapability` hides Lake's shared-library facet, Cargo rerun triggers, cache,
filename convention, and dylib-path env-var plumbing. `LeanCapability` keeps the built path
and initializer names together at runtime. Use `build_lake_target` and `LeanLibrary`
directly only for lower-level custom interop.

See the complete shipping recipe at
[`docs/recipes/ship-crate-with-lean.md`](https://github.com/jcreinhold/lean-rs/blob/main/docs/recipes/ship-crate-with-lean.md).

For a complete low-level example that also lets Lean call a Rust callback, run
`cargo run -p lean-rs --example interop_callback` in the workspace and read
[`docs/recipes/downstream-interop.md`](https://github.com/jcreinhold/lean-rs/blob/main/docs/recipes/downstream-interop.md).
For a trusted same-process string callback example, run
`cargo run -p lean-rs --example string_streaming` and read
[`docs/recipes/string-callback-streaming.md`](https://github.com/jcreinhold/lean-rs/blob/main/docs/recipes/string-callback-streaming.md).
Worker-style tools that need process isolation, live rows, diagnostics, terminal summaries,
timeouts, or memory cycling should use `lean-rs-worker` typed commands instead of exposing
callback handles.

The `Args` and `R` generics on `LeanModule::exported` are sealed by the `LeanAbi` /
`LeanArgs` / `DecodeCallResult` traits, so unsupported types fail at compile time rather than
producing wrong decodes at runtime.

## See also

- Workspace overview, architecture docs, and the worked examples that exercise this surface end to end: [`lean-rs` repository](https://github.com/jcreinhold/lean-rs).
- Host stack (sessions, kernel check, `MetaM`): [`lean-rs-host`](https://docs.rs/lean-rs-host).
- Build-script helper: [`lean-toolchain`](https://docs.rs/lean-toolchain).
- Raw FFI escape hatch (advanced): [`lean-rs-sys`](https://docs.rs/lean-rs-sys).
- Diagnostics and tracing: [`docs/diagnostics.md`](https://github.com/jcreinhold/lean-rs/blob/main/docs/diagnostics.md).

## License

Dual-licensed under MIT or Apache-2.0 at your option.
