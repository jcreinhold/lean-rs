# lean-rs

Safe Rust bindings for hosting Lean 4 capabilities. The single safe front door of the
`lean-rs` project: runtime initialization, owned and borrowed object handles, typed
first-order ABI conversions, module loading and exported functions, semantic handles
(`LeanName`, `LeanLevel`, `LeanExpr`, `LeanDeclaration`), and a structured error/diagnostic
boundary.

Ships **zero Lean-side code**. This is the minimum every mainstream Rust binding to a
GC-hosted language follows (`ocaml-rs`, `hs-bindgen`, `caml-oxide`). If
you want an opinionated theorem-prover-host stack with sessions, `MetaM`, and kernel-checked
evidence, add [`lean-rs-host`](https://docs.rs/lean-rs-host) on top.

## Quick start

```toml
[dependencies]
lean-rs = "0.1"
```

```rust,ignore
use lean_rs::{LeanRuntime, LeanLibrary, LeanModule};

fn main() -> lean_rs::LeanResult<()> {
    let runtime = LeanRuntime::init()?;
    let library = LeanLibrary::open(runtime, "/path/to/lib<pkg>_<MyCapability>.dylib")?;
    let module = library.initialize_module("<pkg>", "MyCapability")?;

    // Drive a user-authored `@[export]` declaration with typed arguments.
    let f = module.exported::<(u64, u64), u64>("my_pkg_add")?;
    let n = f.call((40, 2))?;
    assert_eq!(n, 42);
    Ok(())
}
```

The `Args` and `R` generics on `LeanModule::exported` are sealed by the `LeanAbi` /
`LeanArgs` / `DecodeCallResult` traits, so unsupported types fail at compile time rather than
producing wrong decodes at runtime.

## See also

- Workspace overview and architecture docs: [`lean-rs` repository](https://github.com/jcreinhold/lean-rs).
- Raw FFI escape hatch: [`lean-rs-sys`](https://docs.rs/lean-rs-sys).
- Build-script helpers: [`lean-toolchain`](https://docs.rs/lean-toolchain).
- Diagnostics and tracing: [`docs/diagnostics.md`](https://github.com/jcreinhold/lean-rs/blob/main/docs/diagnostics.md).

## License

Dual-licensed under MIT or Apache-2.0 at your option.
