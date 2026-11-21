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

private unsafe def checkProjectOutline : IO Unit := do
  let dependency := `LeanRsHostShims.Interop
  let olean ← findOLean dependency
  let setupFor (path : System.FilePath) : ModuleSetup := {
    name := `OutlineFile
    importArts := ({} : NameMap ImportArtifacts).insert dependency (.ofArrays #[#[path]]) }
  let source := "import LeanRsHostShims.Interop\nprivate def hiddenCallback := LeanRsFixture.Interop.callbackLoop\n"
  let run := fun setup text (bytes : Nat) => do
    Lean.enableInitializersExecution
    declarationOutlineWithSetup setup text ⟨65536, bytes⟩ "Example" "OutlineFile.lean" 200000 65536 "{}"
  let .ok result imports ← run (setupFor olean) source 65536 | throw (IO.userError "project outline failed")
  let #[.ok "declarations" (.declarationOutline outline)] := result.items | throw (IO.userError "expected outline")
  let #[row] := outline.declarations | throw (IO.userError "expected exactly the file's private declaration")
  unless imports == #[dependency.toString] && row.shortName == "hiddenCallback" &&
      isPrivateName row.declarationName.toName && privateToUserName row.declarationName.toName == `Example.hiddenCallback &&
      row.namespaceName == "Example" && row.declarationSpan.startLine == 2 &&
      !outline.truncated && !result.totalTruncated do throw (IO.userError s!"wrong project outline: {row.declarationName}")
  let .ok clipped _ ← run (setupFor olean) source 1 | throw (IO.userError "small-budget outline failed")
  unless clipped.items matches #[.ok "declarations" (.declarationOutline { declarations := #[], truncated := true })] do
    throw (IO.userError "outline truncation was lost")
  let .headerParseFailed failure ← run (setupFor (olean.addExtension "absent")) source 65536
    | throw (IO.userError "ignored the missing setup-selected artifact")
  unless failure.diagnostics.any (·.severity matches .error) do throw (IO.userError "missing import diagnostic")
  let .ok broken _ ← run (setupFor olean) (source ++ "#check NoSuchFixtureName\n") 65536
    | throw (IO.userError "body error misclassified as a header failure")
  let #[.ok "diagnostics" (.diagnostics errors), .ok "declarations" (.declarationOutline _)] := broken.items
    | throw (IO.userError "body error lost from partial outline")
  unless errors.diagnostics.any (fun d => d.fileLabel == "OutlineFile.lean" && (d.severity matches .error)) do
    throw (IO.userError "missing labeled body diagnostic")

unsafe def main : IO Unit := do
  checkVerdict "fixtureAxiom" true
  checkVerdict "False.intro" false
  checkVerdict "by sorry" false
  checkVerdict "by skip" false
  checkSharedBudget
  checkProjectOutline
