import Lean

namespace LeanRsFixture.SourceRanges

open Lean

theorem knownTheorem : True := by
  trivial

/-- Fixture theorem with docstring and simp metadata for declaration inspection. -/
@[simp] theorem documentedSimpTheorem (p : Prop) : (p ∧ True) ↔ p := by
  simp

set_option backward.privateInPublic true in
private def privateFixture : Nat := 1

run_cmd do
  let ns := `LeanRsFixture.SourceRanges
  let mkDef (name : Name) (value : Nat) : Declaration :=
    .defnDecl {
      name
      levelParams := []
      type := mkConst ``Nat
      value := mkNatLit value
      hints := .abbrev
      safety := .safe
    }
  let privateName := mkPrivateNameCore `LeanRsFixture.SourceRanges (ns ++ `privateSynthetic)
  Lean.Elab.Command.liftCoreM <| addDecl <| mkDef (ns ++ `syntheticNoRange) 0
  -- `addDecl` (not the lower-level `Environment.addDeclCore`) so this compiles
  -- across the whole supported window: 4.33.0-rc1 inserted a second `USize`
  -- parameter into `addDeclCore`, breaking a positional call. `addDecl` adds
  -- the same private synthetic constant with no source range, exactly as for
  -- the sibling declarations below.
  Lean.Elab.Command.liftCoreM <| addDecl <| mkDef privateName 1
  Lean.Elab.Command.liftCoreM <| addDecl <| mkDef (.num (ns ++ `generatedFixture) 0) 1
  Lean.Elab.Command.liftCoreM <| addDecl <| mkDef (ns ++ `_internalFixture) 2

end LeanRsFixture.SourceRanges
