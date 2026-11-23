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

private unsafe def checkProjectFileQueries : IO Unit := do
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

  let messages := source ++ "#check hiddenCallback\nprivate theorem unfinished : True := by sorry\n"
  let diagnostics := fun setup (bytes : Nat) (diagBytes : USize) => do
    Lean.enableInitializersExecution
    fileDiagnosticsWithSetup setup messages ⟨65536, bytes⟩ "Example" "OutlineFile.lean" 200000 diagBytes "{}"
  let .ok reported imports ← diagnostics (setupFor olean) 65536 65536 | throw (IO.userError "diagnostics failed")
  let #[.ok "diagnostics" (.diagnostics log)] := reported.items | throw (IO.userError "expected typed diagnostics")
  unless imports == #[dependency.toString] && !reported.totalTruncated && (log.truncated matches .complete) &&
      log.diagnostics.any (fun d => (d.severity matches .info) && d.position.any (·.line == 3)) &&
      log.diagnostics.any (fun d => (d.severity matches .warning) && d.position.any (·.line == 4)) &&
      log.diagnostics.all (fun d => d.fileLabel == "OutlineFile.lean" && !(d.severity matches .error)) do
    throw (IO.userError "missing project warning or information at the original source position")
  let .ok limited _ ← diagnostics (setupFor olean) 65536 1 | throw (IO.userError "limited diagnostics failed")
  unless limited.items matches #[.ok "diagnostics" (.diagnostics { truncated := .truncated, .. })] do
    throw (IO.userError "diagnostic truncation was lost")
  let .ok exhausted _ ← diagnostics (setupFor olean) 1 65536 | throw (IO.userError "limited batch failed")
  unless exhausted.totalTruncated && (exhausted.items matches #[.budgetExceeded "diagnostics" _]) do
    throw (IO.userError "diagnostics exceeded the batch budget")
  let .headerParseFailed failure ← diagnostics (setupFor (olean.addExtension "absent")) 65536 65536
    | throw (IO.userError "diagnostics ignored the missing setup-selected artifact")
  unless failure.diagnostics.any (·.severity matches .error) do throw (IO.userError "missing import diagnostic")

  let .headerParseFailed tiny ← diagnostics (setupFor (olean.addExtension "absent")) 1 65536
    | throw (IO.userError "small header budget hid the import failure")
  unless tiny.diagnostics.isEmpty && (tiny.truncated matches .truncated) do
    throw (IO.userError "unreportable header error was marked complete")

  let atHostBudget := fun text => do
    Lean.enableInitializersExecution
    fileDiagnosticsWithSetup (setupFor olean) text ⟨4096, 65536⟩ "Example" "OutlineFile.lean" 200000 65536 "{}"
  let .ok clean _ ← atHostBudget source | throw (IO.userError "clean file failed")
  unless clean.items matches #[.ok "diagnostics" (.diagnostics { diagnostics := #[], truncated := .complete })] do
    throw (IO.userError "clean file did not return complete empty diagnostics")
  -- First exhaust only the host's diagnostic body ceiling, then exceed its whole batch ceiling.
  for noiseChars in [3000, 35000] do
    let noise := String.ofList (List.replicate noiseChars 'λ')
    let noisy := messages ++ "#check \"" ++ noise ++ "\"\n#check MissingFirst\n#check MissingSecond\n"
    let .ok prioritized _ ← atHostBudget noisy | throw (IO.userError "noisy file failed")
    let #[.ok "diagnostics" (.diagnostics bounded)] := prioritized.items
      | throw (IO.userError "noisy output displaced the typed diagnostics item")
    let some first := bounded.diagnostics[0]? | throw (IO.userError "missing first error")
    let some second := bounded.diagnostics[1]? | throw (IO.userError "missing second error")
    unless (bounded.truncated matches .truncated) &&
        (first.severity matches .error) && first.message.contains "MissingFirst" && first.position.any (·.line == 6) &&
        (second.severity matches .error) && second.message.contains "MissingSecond" && second.position.any (·.line == 7) &&
        bounded.diagnostics.filterMap (fun d => d.position.map (·.line)) == #[6, 7, 4, 3, 5] &&
        bounded.diagnostics.all (·.fileLabel == "OutlineFile.lean") &&
        bounded.diagnostics.foldl (init := 0) (fun n d => n + d.message.utf8ByteSize) ≤ 4096 do
      throw (IO.userError "bounded diagnostics lost severity order, positions, or truncation")

private unsafe def checkSetupProofState : IO Unit := do
  let dependency := `LeanRsHostShims.Interop
  let olean ← findOLean dependency
  let source := "import LeanRsHostShims.Interop\ntheorem positioned : True := by\n  exact True.intro\n" ++
    "#check MissingProofStateFixture\n"
  let run := fun (path : System.FilePath) (position : ProofPositionSelector) (total : Nat) =>
    IO.FS.withTempFile fun file staged => do
      let setup : ModuleSetup := {
        name := `Positioned
        importArts := ({} : NameMap ImportArtifacts).insert dependency (.ofArrays #[#[path]]) }
      file.putStr (toJson setup).compress
      file.flush
      let loaded ← ModuleSetup.load staged
      Lean.enableInitializersExecution
      fileProofStateWithSetup loaded source "positioned" position ⟨4096, total⟩ "" "Positioned.lean" 200000 4096 "{}"
  let .ok result imports ← run olean .entry 65536 | throw (IO.userError "setup proof state failed")
  let #[.ok "proof_state" (.proofState (.state state)), .ok "diagnostics" (.diagnostics log)] := result.items
    | throw (IO.userError "missing paired proof state and diagnostics")
  unless imports == #[dependency.toString] && !result.totalTruncated &&
      state.declarationName == some "positioned" && state.span.startLine == 3 &&
      state.goalsBefore.any (·.contains "⊢ True") && state.goalsAfter == state.goalsBefore do
    throw (IO.userError "wrong goal or position for staged source")
  unless log.diagnostics.any (fun d => (d.severity matches .error) && d.fileLabel == "Positioned.lean" &&
      d.position.any (·.line == 4) && d.message.contains "MissingProofStateFixture") do
    throw (IO.userError "missing body diagnostic from the same source")
  let .ok after _ ← run olean (.index 0) 65536 | throw (IO.userError "indexed proof state failed")
  let #[.ok "proof_state" (.proofState (.state indexed)), .ok "diagnostics" _] := after.items
    | throw (IO.userError "missing indexed proof state")
  unless indexed.goalsBefore == state.goalsBefore && indexed.goalsAfter.isEmpty do
    throw (IO.userError "index did not select the post-tactic state")
  let .ok limited _ ← run olean .entry 1 | throw (IO.userError "tiny proof-state projection failed")
  unless limited.totalTruncated && (limited.items matches
      #[.budgetExceeded "proof_state" _, .budgetExceeded "diagnostics" _]) do
    throw (IO.userError "proof-state batch exceeded the shared output budget")
  let .headerParseFailed failure ← run (olean.addExtension "absent") .entry 65536
    | throw (IO.userError "proof state ignored the staged setup's missing artifact")
  unless failure.diagnostics.any (·.severity matches .error) do
    throw (IO.userError "missing import refusal diagnostic")

private unsafe def checkSetupProofAttempt : IO Unit := IO.FS.withTempDir fun dir => do
  let dependency := `StagedTrialFixtureDependency
  let olean := dir / "StagedTrialFixtureDependency.olean"
  IO.FS.writeFile (dir / "StagedTrialFixtureDependency.lean") "theorem stagedTrialFact : True := True.intro\n"
  let compiler := (← findSysroot) / "bin" / "lean"
  let built ← IO.Process.output {
    cmd := compiler.toString
    cwd := some dir
    args := #["-o", olean.toString, "StagedTrialFixtureDependency.lean"] }

  unless built.exitCode == 0 && (← (← searchPathRef.get).findRootWithExt "olean" dependency).isNone do
    throw (IO.userError s!"could not isolate staged import: {built.stderr}")
  let request : ProofAttemptRequest := {
    source := "import StagedTrialFixtureDependency\ntheorem target : True := by\n  exact True.intro\n" ++
      "#check MissingTrialDownstream\n"
    edit := .declaration "target" .entry
    candidates := #[⟨"closed", "exact stagedTrialFact"⟩, ⟨"failed", "exact MissingTrialCandidate"⟩]
    budgets := ⟨4096, 65536⟩ }
  let run := fun (path : System.FilePath) => do
    let setup : ModuleSetup := {
      name := `StagedTrial
      importArts := ({} : NameMap ImportArtifacts).insert dependency (.ofArrays #[#[path]]) }
    let staged := dir / "module.setup.json"
    IO.FS.writeFile staged (toJson setup).compress
    let loaded ← ModuleSetup.load staged
    Lean.enableInitializersExecution
    attemptProofWithSetup loaded request "" "StagedTrial.lean" 200000 4096 "{}"
  let .ok envelope imports ← run olean | throw (IO.userError "staged trial failed")
  let #[closed, failed] := envelope.candidates | throw (IO.userError "missing staged candidates")
  unless imports == #[dependency.toString] && envelope.entryGoals.any (·.value.contains "⊢ True") &&
      closed.id == "closed" && (closed.status matches .closed) &&
      failed.id == "failed" && (failed.status matches .failed) do
    throw (IO.userError s!"staged trial: imports={imports}, entry={envelope.entryGoals.map (·.value)},       closed={(closed.status matches .closed)}, failed={(failed.status matches .failed)},       diagnostics={closed.diagnostics.diagnostics.map (·.message)},       downstream={closed.downstreamDiagnostics.diagnostics.map (·.message)}")
  unless closed.diagnostics.diagnostics.all (fun d => !(d.severity matches .error)) &&
      failed.diagnostics.diagnostics.any (fun d => (d.severity matches .error) &&
        d.message.contains "MissingTrialCandidate") &&
      failed.downstreamDiagnostics.diagnostics.any (fun d => d.message.contains "MissingTrialDownstream") do
    throw (IO.userError "staged candidate and downstream diagnostics were conflated")
  let .headerParseFailed failure ← run (olean.addExtension "absent")
    | throw (IO.userError "trial ignored a missing staged import")
  unless failure.diagnostics.any (·.severity matches .error) do
    throw (IO.userError "missing staged import refusal diagnostic")

unsafe def main : IO Unit := do
  checkVerdict "fixtureAxiom" true
  checkVerdict "False.intro" false
  checkVerdict "by sorry" false
  checkVerdict "by skip" false
  checkSharedBudget
  checkProjectFileQueries
  checkSetupProofState
  checkSetupProofAttempt
