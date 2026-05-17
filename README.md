# lean-rs

Rust bindings for hosting [Lean 4](https://lean-lang.org/) capabilities. Lean owns Lean semantics — elaboration,
kernel checking, proof objects, universes, `MetaM`, and dependent-type meaning. This project owns hosting: linking,
runtime initialization, ABI conversion, module loading, error and panic boundaries, scheduling, diagnostics,
batching, and packaging. Rust does not reconstruct Lean semantic facts; that responsibility stays in Lean.

The project is at the workspace-bootstrap stage. Public APIs are deliberately empty until the relevant prompts in the
implementation sequence land them.

## Workspace layout

Two published crates plus two workspace-internal helpers. Raw Lean 4 C ABI bindings live in the in-tree
`lean-rs-sys` crate (`publish = false`); the rest of the workspace builds on top of it. See
`docs/architecture/00-charter.md` and `RD-2026-05-17-003` in
[`prompts/lean-rs/00-current-state.md`](https://github.com/jcreinhold/lean-rs) for why raw FFI is in-tree rather
than adopted from an external crate.

| Crate                       | Published | Role                                                                                       |
| --------------------------- | --------- | ------------------------------------------------------------------------------------------ |
| `lean-rs-sys`               | no        | In-tree raw Lean 4 C ABI bindings: curated `extern "C"` declarations, hand-written refcount inline helpers, signature-checked symbol allowlist, header SHA-256 digest, and link directives. `publish = false`. |
| `lean-toolchain`            | yes       | Lean toolchain discovery, typed fingerprint, fixture digest, layered link diagnostics, and build-script helpers that downstream embedders can call from their own `build.rs`. |
| `lean-rs`                   | yes       | The single safe front door: runtime initialization, owned/borrowed object handles, typed ABI conversions, module loading, exported functions, semantic handles, bounded meta services, batching, and session pooling. |
| `lean-rs-test-support`      | no        | Workspace-internal fixtures and helpers. `publish = false`.                                |

The layering invariant is `lean-rs-sys` → `lean-toolchain` → `lean-rs`. Raw `lean_object *` and raw `lean_*` symbols
enter the workspace only via `lean-rs-sys` and are not re-exported by `lean-toolchain` or `lean-rs`. Internal
organization of `lean-rs` mirrors the original layer story (`lean_rs::runtime`, `lean_rs::abi`, `lean_rs::module`,
`lean_rs::host`, `lean_rs::batch`) but those boundaries are policed by `pub(crate)` rather than crate splits, so they
can be reorganized without semver breakage.

## Architecture

Architecture and policy docs live under [`docs/architecture/`](docs/architecture/):

- [`00-charter.md`](docs/architecture/00-charter.md) — design boundary, hidden knowledge, smallest public
  interface, and the design-it-twice record of rejected alternatives.
- [`01-safety-model.md`](docs/architecture/01-safety-model.md) — unsafe boundary thesis, reference-counting
  ownership, proof-object opacity, concurrency stance, and the workspace `unsafe-code = "deny"` lint policy.
- [`02-versioning-and-compatibility.md`](docs/architecture/02-versioning-and-compatibility.md) — supported Lean
  toolchain range, in-tree raw-FFI policy, header digest policy, crate semver, and supported-platform list.

Later prompts in the implementation sequence read these docs first when deciding whether an API is deep, safe,
and semantically honest.

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
