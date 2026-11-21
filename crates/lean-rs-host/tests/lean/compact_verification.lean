import LeanRsHostShims.InfoTree

open Lean LeanRsFixture.InfoTree

private unsafe def checkVerdict (proof : String) (accepted : Bool) : IO Unit := do
  let axiomName := "fixtureAxiom" ++ String.ofList (List.replicate 240 'a')
  let noise := String.ofList (List.replicate 70000 'x')
  let source := "import Init\n#check \"" ++ noise ++ "\"\naxiom " ++ axiomName ++ " : True\n" ++
    "theorem fixtureTarget : True := " ++ (if accepted then axiomName else proof)
  let request : DeclarationVerificationBatchRequest := {
    source, targets := #[⟨"row", .name "fixtureTarget"⟩], sorryPolicy := 1, reportAxioms := 1
    budgets := ⟨262144, 262144⟩ }
  let setup : ModuleSetup := { name := `Fixture }
  Lean.enableInitializersExecution
  let .ok #[reference] _ ← verifyDeclarationBatchWithSetup setup request "" "Fixture.lean" 200000 65536 "{}"
    | throw (IO.userError "missing full reference row")
  let small := { request with budgets := ⟨1024, 1024⟩ }
  Lean.enableInitializersExecution
  let .ok #[full] _ ← verifyDeclarationBatchWithSetup setup small "" "Fixture.lean" 200000 65536 "{}"
    | throw (IO.userError "missing bounded full row")
  unless full.status matches .budgetExceeded do throw (IO.userError "full overflow control failed")
  Lean.enableInitializersExecution
  let .ok #[row] _ ← verifyDeclarationBatchCompactWithSetup setup small "" "Fixture.lean" 200000 65536 "{}"
    | throw (IO.userError "missing compact row")
  let expected := fun (status : DeclarationVerificationStatus) => match status with
    | .accepted => accepted
    | .rejected => !accepted
    | _ => false
  unless expected reference.status && expected row.status do throw (IO.userError "status mismatch")
  unless row.id == "row" && (row.target matches .name "fixtureTarget") &&
      row.facts.target.any (·.declarationName == "fixtureTarget") do throw (IO.userError "identity mismatch")
  unless row.facts.axiomsAvailable == reference.facts.axiomsAvailable &&
      row.facts.axiomsTruncated == reference.facts.axiomsTruncated &&
      row.facts.axioms == reference.facts.axioms do throw (IO.userError "axiom mismatch")
  unless row.facts.diagnostics.truncated matches .truncated do
    throw (IO.userError "omitted diagnostic was not marked truncated")
  unless row.facts.unresolvedGoals.isEmpty == reference.facts.unresolvedGoals.isEmpty do
    throw (IO.userError "unresolved goal mismatch")
  if proof == "False.intro" then
    unless row.facts.diagnostics.diagnostics.any (·.severity matches .error) do
      throw (IO.userError "oversized info hid the actionable error")
  if accepted then
    unless row.facts.axiomsAvailable && !row.facts.axiomsTruncated && row.facts.axioms.contains axiomName do
      throw (IO.userError "missing known axiom")
    let tight := { small with budgets := ⟨200, 200⟩ }
    Lean.enableInitializersExecution
    let .ok #[limited] _ ← verifyDeclarationBatchCompactWithSetup setup tight "" "Fixture.lean" 200000 65536 "{}"
      | throw (IO.userError "missing overflow refusal")
    unless (limited.status matches .accepted) && limited.facts.axiomsAvailable &&
        limited.facts.axiomsTruncated && limited.facts.axioms.isEmpty do
      throw (IO.userError "overflow reported complete facts")

private unsafe def checkSharedBudget : IO Unit := do
  let noise := String.ofList (List.replicate 800 'x')
  let source := "import Init\n#check \"" ++ noise ++ "\"\n" ++
    "theorem fixtureFirst : True := True.intro\ntheorem fixtureSecond : True := True.intro\n"
  let request : DeclarationVerificationBatchRequest := {
    source, targets := #[⟨"first", .name "fixtureFirst"⟩, ⟨"second", .name "fixtureSecond"⟩]
    sorryPolicy := 1, reportAxioms := 0, budgets := ⟨1024, 1024⟩ }
  let setup : ModuleSetup := { name := `Fixture }
  Lean.enableInitializersExecution
  let .ok #[first, second] _ ← verifyDeclarationBatchCompactWithSetup setup request "" "Fixture.lean"
      200000 65536 "{}"
    | throw (IO.userError "missing compact batch rows")
  unless (first.status matches .accepted) && (second.status matches .accepted) &&
      first.id == "first" && second.id == "second" && first.facts.outputTruncated do
    throw (IO.userError "display evidence exhausted the shared verdict budget")

unsafe def main : IO Unit := do
  checkVerdict "fixtureAxiom" true
  checkVerdict "False.intro" false
  checkVerdict "by sorry" false
  checkVerdict "by skip" false
  checkSharedBudget
