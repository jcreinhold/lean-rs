import Lean

/-! A module whose `initialize` block registers a scoped environment extension,
    reachable **only** through `Lean.importModules`.

    Its own `lean_lib`, deliberately unreachable from the `LeanRsFixture`
    roll-up. `LeanCapability` runs a loaded root module's initializers eagerly
    with `builtin = 1`, and by then `LeanRuntime::init` has already called
    `lean_io_mark_end_initialization()`, so `Lean.registerEnvExtension` throws
    unless `Lean.initializing` holds — putting this module in the roll-up would
    break every capability-mode fixture test at load time.

    Reached through an import instead, this block runs inside
    `Lean.withImporting`, where `importingRef` makes `Lean.initializing` true and
    registration is legal. Registering there grows the process-global extension
    registry underneath every environment that already exists, which is exactly
    the condition a pooled session must never be allowed to serve a query from.
    That is the whole point of this fixture. -/
namespace LeanRsFixture.LateExtension

open Lean

initialize lateScopedExtension : SimpleScopedEnvExtension Name (Array Name) ←
  registerSimpleScopedEnvExtension {
    addEntry := fun entries name => entries.push name
    initial := #[]
  }

end LeanRsFixture.LateExtension
