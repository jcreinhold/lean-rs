# lean-rs

Rust bindings for hosting [Lean 4](https://lean-lang.org/) capabilities. Lean owns elaboration, kernel checking, proof
objects, universes, `MetaM`, and dependent-type meaning; this project owns linking, runtime initialization, ABI
conversion, module loading, error and panic boundaries, scheduling, diagnostics, batching, and packaging.

The project is at the workspace-bootstrap stage. Public APIs are deliberately empty until the relevant prompts in the
implementation sequence land them.

## Workspace layout

Three published crates plus one workspace-internal helper. Raw Lean 4 C ABI bindings are provided by the external
[`lean-sys`](https://crates.io/crates/lean-sys) crate (digama0/Mario Carneiro); this project builds on top of it
rather than re-implementing ~196 hand-written `extern "C"` declarations.

| Crate                       | Published | Role                                                                                       |
| --------------------------- | --------- | ------------------------------------------------------------------------------------------ |
| [`lean-sys`](https://crates.io/crates/lean-sys) *(external)* | yes | Raw Lean 4 C ABI bindings. Workspace dependency, not maintained here.        |
| `lean-toolchain`            | yes       | Lean toolchain discovery, fingerprinting, symbol allowlist, build-script helpers that downstream embedders can call from their own `build.rs`. |
| `lean-rs`                   | yes       | The single safe front door: runtime initialization, owned/borrowed object handles, typed ABI conversions, module loading, exported functions, semantic handles, bounded meta services, batching, and session pooling. |
| `lean-rs-test-support`      | no        | Workspace-internal fixtures and helpers. `publish = false`.                                |

The layering invariant is `lean-sys` → `lean-toolchain` → `lean-rs`. Raw `lean_object *` and raw `lean_*` symbols
enter the workspace only via `lean-sys` and are not re-exported by `lean-toolchain` or `lean-rs`. Internal organization
of `lean-rs` mirrors the original layer story (`lean_rs::runtime`, `lean_rs::abi`, `lean_rs::module`, `lean_rs::host`,
`lean_rs::batch`) but those boundaries are policed by `pub(crate)` rather than crate splits, so they can be reorganized
without semver breakage.

## Build

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo doc --no-deps
```

## License

Dual-licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or <http://www.apache.org/licenses/LICENSE-2.0>)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or <http://opensource.org/licenses/MIT>)

at your option. See [CONTRIBUTING.md](CONTRIBUTING.md) for contribution rules, including unsafe-code and
Lean-version-compatibility expectations.
