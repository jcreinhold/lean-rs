# lean-rs

Rust bindings for hosting [Lean 4](https://lean-lang.org/) capabilities. Lean owns Lean semantics — elaboration, kernel
checking, proof objects, universes, `MetaM`, and dependent-type meaning. This project owns hosting: linking, runtime
initialization, ABI conversion, module loading, error and panic boundaries, scheduling, diagnostics, batching, and
packaging. Rust does not reconstruct Lean semantic facts; that responsibility stays in Lean.

The project is at the workspace-bootstrap stage. Public APIs are deliberately empty until the relevant prompts in the
implementation sequence land them.

## Workspace layout

Three published crates plus one workspace-internal helper. Raw Lean 4 C ABI bindings live in the in-tree `lean-rs-sys`
crate; the rest of the workspace builds on top of it. See `docs/architecture/00-charter.md`, `RD-2026-05-17-003`
(in-tree raw FFI), and `RD-2026-05-17-005` (`lean-rs-sys` published with opaque public types) in
[`prompts/lean-rs/00-current-state.md`](https://github.com/jcreinhold/lean-rs) for the design rationale.

| Crate                  | Published | Role                                                                                                                                                                                                                                                                                                                                                   |
| ---------------------- | --------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `lean-rs-sys`          | yes       | Raw Lean 4 C ABI bindings: curated `extern "C"` declarations split by semantic category, pure-Rust mirrors of `lean.h`'s `static inline` refcount helpers, `REQUIRED_SYMBOLS` allowlist, header digest. Public types (`lean_object`) are opaque; layout is `pub(crate)`. Opt-in unsafe raw FFI; the safe layers in `lean-rs` are the recommended path. |
| `lean-toolchain`       | yes       | Lean toolchain discovery, typed fingerprint, fixture digest, layered link diagnostics, and build-script helpers that downstream embedders can call from their own `build.rs`.                                                                                                                                                                          |
| `lean-rs`              | yes       | The single safe front door: runtime initialization (token-bound `'lean` lifetime), owned/borrowed object handles (internal), typed ABI conversions (internal), module loading, typed exported functions, semantic handles, bounded meta services, and `LeanSession` bulk/pool operations.                                                              |
| `lean-rs-test-support` | no        | Workspace-internal fixtures and helpers (`publish = false`).                                                                                                                                                                                                                                                                                           |

The layering invariant is `lean-rs-sys` → `lean-toolchain` → `lean-rs`. Raw `lean_object *` and raw `lean_*` symbols
enter the workspace only via `lean-rs-sys` and are not re-exported by `lean-toolchain` or `lean-rs`. Internal
organization of `lean-rs` is three publicly visible modules (`lean_rs::module`, `lean_rs::host`, `lean_rs::error`) plus
two `pub(crate)` infrastructure modules (`runtime`, `abi`); batch and pool operations are methods on `LeanSession`
rather than a sibling module. Boundaries are policed by `pub(crate)` rather than crate splits, so they can be
reorganized without semver breakage. See `docs/architecture/03-host-api.md` for the curated public surface and
`RD-2026-05-17-004` in `prompts/lean-rs/00-current-state.md` for the design rationale.

## Architecture

Architecture and policy docs live under [`docs/architecture/`](docs/architecture/):

- [`00-charter.md`](docs/architecture/00-charter.md) — design boundary, hidden knowledge, smallest public interface, and
    the design-it-twice record of rejected alternatives.
- [`01-safety-model.md`](docs/architecture/01-safety-model.md) — unsafe boundary thesis, reference-counting ownership,
    proof-object opacity, concurrency stance, and the workspace `unsafe-code = "deny"` lint policy.
- [`02-versioning-and-compatibility.md`](docs/architecture/02-versioning-and-compatibility.md) — supported Lean
    toolchain range, in-tree raw-FFI policy, header digest policy, crate semver, and supported-platform list.
- [`04-raw-sys-design.md`](docs/architecture/04-raw-sys-design.md) — per-decision rationale behind `lean-rs-sys`
    (publication status, opaque types, refcount-mirror strategy, module layout, naming, minimum-unsafe discipline).

Later prompts in the implementation sequence read these docs first when deciding whether an API is deep, safe, and
semantically honest.

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
