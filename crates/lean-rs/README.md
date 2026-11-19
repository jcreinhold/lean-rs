# lean-rs

Safe Rust bindings for hosting Lean 4 capabilities. This crate covers runtime initialization, owned and borrowed object
handles, typed first-order ABI conversions, module loading and exported functions, semantic handles (`LeanName`,
`LeanLevel`, `LeanExpr`, `LeanDeclaration`), and a structured error/diagnostic boundary.

Ships the generic interop shim package used by same-process Lean-to-Rust callbacks, but no theorem-prover host shims. If
you want sessions, `MetaM`, and kernel-checked evidence, add [`lean-rs-host`](https://docs.rs/lean-rs-host) on top.

## Quick start

The shipped-crate template builds its own Lean source using the package-private Lake bootstrap. From the workspace,
run the standalone consumer with the matching Lean toolchain selected:

```sh
cargo run --manifest-path templates/shipped-lean-crate/Cargo.toml
```

The program prints `answer=42`. Its build script records the compiled `ShipLeanDemo` image, initializer and
`UInt64 × UInt64 → UInt64` export. At runtime the application checks the captured manifest, stages its own image
bytes in a private directory and admits that exact export under the matching runtime. The caller can then use
`capability.bind_binary("ship_lean_demo_add")?.call(40, 2)?`.
[Build script](https://github.com/jcreinhold/lean-rs/blob/main/templates/shipped-lean-crate/build.rs) ·
[Rust caller](https://github.com/jcreinhold/lean-rs/blob/main/templates/shipped-lean-crate/src/main.rs)

The Lean source defines the exported name and the types Rust must use:

```lean
@[export ship_lean_demo_add]
def add (a b : UInt64) : UInt64 := a + b
```

The Lake package makes `ShipLeanDemo` a shared target:

```lean
import Lake
open Lake DSL

package «ship_lean_demo»

@[default_target]
lean_lib «ShipLeanDemo» where
  defaultFacets := #[LeanLib.sharedFacet]
```

For the Cargo dependencies, Lake target and app-owned worker child, see the
[shipping recipe](https://github.com/jcreinhold/lean-rs/blob/main/docs/recipes/ship-crate-with-lean.md).
For changes from the 0.9 loader API, see
[native ownership in 0.10](https://github.com/jcreinhold/lean-rs/blob/main/docs/architecture/loading.md#native-ownership-in-010).

The `interop_callback` and `string_streaming` examples show synchronous Lean-to-Rust callbacks with an admitted
fixture pair. For their artifact and callback lifetime requirements, see
[downstream interop](https://github.com/jcreinhold/lean-rs/blob/main/docs/recipes/downstream-interop.md).
Worker applications use typed commands through `lean-rs-worker-parent` for live rows and process isolation.

Generic export binding and calls require the application's unsafe assertion about the actual native code.
Custom implementations of `LeanAbi` must uphold representation, ownership and runtime requirements in both
directions. [Safety model](https://github.com/jcreinhold/lean-rs/blob/main/docs/architecture/safety.md)

## See also

- Workspace overview, architecture docs, and the worked examples that exercise this surface end to end:
  [`lean-rs` repository](https://github.com/jcreinhold/lean-rs).
- Standard Lean services (sessions, kernel check, `MetaM`): [`lean-rs-host`](https://docs.rs/lean-rs-host).
- Build-script helper: [`lean-toolchain`](https://docs.rs/lean-toolchain).
- Raw FFI escape hatch (advanced): [`lean-rs-sys`](https://docs.rs/lean-rs-sys).
- Diagnostics and tracing: [`docs/diagnostics.md`](https://github.com/jcreinhold/lean-rs/blob/main/docs/diagnostics.md).

## License

Dual-licensed under MIT or Apache-2.0 at your option.
