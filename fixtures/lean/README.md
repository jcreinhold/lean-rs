# Lean ABI Fixtures

`lean_rs_fixture` is the Lake package whose `LeanRsFixture` library is loaded by workspace tests, benchmarks, and
examples. Its exported symbols use the `lean_rs_fixture_` prefix; changing a symbol requires updating its Rust callers.

| Module | Coverage |
| --- | --- |
| `Scalars` | `UInt8`/`16`/`32`/`64`, `USize`, `Nat`, `Int`, `Bool`, `Unit`, `Char`, `Float` |
| `Strings` | `String`, `ByteArray` |
| `Containers` | `Array String`, `Option Nat`, `Except String Nat`, and a two-field structure |
| `Effects` | `IO` success (`Unit`, `Nat`), inner `Except` failure, and thrown `IO` errors |
| `Evidence` | A `Prop` witness returned as an opaque handle |
| `Capability` | Imported `CoreM`/`MetaM` declarations, without direct exports |
| `Handles` | Name, level, expression, and declaration handles |
| `Meta` | Expressions used by bounded meta-service tests |
| `SourceRanges` | Source positions, docstrings, attributes, and declarations without ranges |

[`LeanRsFixtureLateExtension.lean`](LeanRsFixtureLateExtension.lean) is a separate default library target. Its
initializer registers an environment extension during `Lean.importModules`; keep it out of the roll-up library so
capability loading does not run that initializer too early.

## Build and use

```sh
cd fixtures/lean
lake build
```

Lake writes the shared library and serialized `.olean` modules under `.lake/build/`. Its underscore escaping turns
package `lean_rs_fixture` into `lean__rs__fixture`, giving a shared-library stem `liblean__rs__fixture_LeanRsFixture`
and initializer `initialize_lean__rs__fixture_LeanRsFixture`. Ask Lake for artifact paths through
`lean_toolchain::LakeWorkspace` or `CargoLeanCapability` rather than hard-coding a platform extension or output
subdirectory.

The [lakefile](lakefile.lean) shows the shared-library target shape.

The fixture does not import host shims. `lean-rs-host` builds and loads its bundled shims separately, preserving the
fixture's use by L1 tests that open the dylib directly. `CoreM` and `MetaM` services cross the ABI through those bounded
host shims, rather than as monadic compiler state in this fixture's export signatures.
