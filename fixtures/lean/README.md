# `LeanRsFixture`—ABI Boundary Fixtures

In-tree Lake package the workspace's tests, benchmarks, and example binaries load. It exists so the `lean-rs` Rust
crates have a stable set of compiled Lean symbols to call when exercising every distinct ABI behavior at the C boundary.

`lakefile.lean` here also doubles as a template for a consumer's own Lake package—the structure transfers verbatim.
Rename the package and module to taste, then add your own `@[export]` declarations. `lean-rs-host` ships its host shims
separately; consumer Lake packages do not require them.

## What's exported

Each submodule under `LeanRsFixture/` covers one ABI category:

| Submodule | Covers |
| --- | --- |
| `Scalars` | `UInt8`/`16`/`32`/`64`, `USize`, `Nat`, `Int`, `Bool`, `Unit`, `Char`, `Float` |
| `Strings` | `String`, `ByteArray` |
| `Containers` | `Array String`, `Option Nat`, `Except String Nat`, a two-field structure |
| `Effects` | `IO` success (`Unit`, `Nat`), `IO` inner `Except` failure, `IO` exception via `throw` |
| `Evidence` | A structure carrying a `Prop` witness, surfaced to Rust as an opaque handle |
| `Capability` | `CoreM`/`MetaM` declarations compiled (via `import Lean`) but never exported |

Every exported symbol is prefixed `lean_rs_fixture_`. Renaming the prefix is a contract change, not a refactor—the
workspace's `@[export]` declarations and Rust call sites are synchronised against it.

## Build

```sh
cd fixtures/lean
lake build
```

Artifacts land under `.lake/build/`:

- `.lake/build/lib/liblean__rs__fixture_LeanRsFixture.{dylib,so}`—the shared library Rust will link.
- `.lake/build/lib/lean/LeanRsFixture/*.olean` and `.lake/build/lib/lean/LeanRsFixture.olean`—per-submodule object
  files.

Lake mangles each underscore in the package name to a double underscore in emitted symbol and filename strings; the
module initializer is therefore `initialize_lean__rs__fixture_LeanRsFixture`. Rust callers derive these names
mechanically from the package name via Lake's mangling rule (`s/_/__/g`).

`lake build` is also the verification command for the contract.

## Why `Capability` has no exports

`MetaM` and `CoreM` carry compiler state (`Environment`, options, traces) that has no meaningful C ABI representation,
so they cannot appear in an `@[export]` signature. The module exists so the package's module-initializer pipeline
imports `Lean`; the `MetaM` capability is exposed to Rust through the bounded `lean_rs_host_meta_*` services declared in
the bundled `lean-rs-host` shim package, not through direct `@[export]` here.
