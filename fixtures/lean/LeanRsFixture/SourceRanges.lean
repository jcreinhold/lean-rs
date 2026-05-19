import Lean

namespace LeanRsFixture.SourceRanges

open Lean

theorem knownTheorem : True := by
  trivial

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
  Lean.Elab.Command.liftCoreM <|
    withOptions (ResolveName.backward.privateInPublic.set · true) <|
      addDecl <| mkDef privateName 1
  Lean.Elab.Command.liftCoreM <| addDecl <| mkDef (.num (ns ++ `generatedFixture) 0) 1
  Lean.Elab.Command.liftCoreM <| addDecl <| mkDef (ns ++ `_internalFixture) 2

end LeanRsFixture.SourceRanges
