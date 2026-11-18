//! Integration coverage for attempt-envelope entry goals and locals.
//!
//! The attempt envelope renders the entry state (the selected tactic's
//! `goalsBefore` and its local hypotheses) once per batch through the same
//! machinery as the proof-position query, so every field must match what
//! `process_module_query_batch` reports at the same position, and degraded or
//! unresolvable entry state must yield empty arrays without blocking the
//! attempt.

#![allow(clippy::expect_used, clippy::indexing_slicing, clippy::panic)]

#[path = "support/fixed_fixture.rs"]
mod fixed_fixture;

use lean_rs::LeanRuntime;
use lean_rs_host::host::process::{
    ModuleQueryBatchItem, ModuleQueryBatchOutcome, ModuleQueryBatchResult, ModuleQueryOutputBudgets,
    ModuleQuerySelector, ProofStateInfo, ProofStateResult,
};
use lean_rs_host::{
    LeanCapabilities, LeanElabOptions, LeanHost, LeanSession, ProofAttemptOutcome, ProofAttemptRequest, ProofCandidate,
    ProofEditTarget, ProofPositionSelector,
};

fn attempt_session<'lean, 'c>(caps: &'c LeanCapabilities<'lean, 'c>) -> LeanSession<'lean, 'c> {
    caps.session(&fixed_fixture::imports(caps, "LeanRsHostShims.Elaboration"), None, None)
        .expect("session imports the bundled elaboration shims")
}

/// Three-tactic proof where the pre-tactic state, post-tactic state, and local
/// hypotheses all differ across positions.
const SOURCE: &str = "\
import Lean

theorem t : ∀ n : Nat, n + 0 = n := by
  intro n
  show n + 0 = n
  exact Nat.add_zero n
";

fn attempt(source: &str, position: ProofPositionSelector, candidates: Vec<ProofCandidate>) -> ProofAttemptOutcome {
    let root = fixed_fixture::fixture_root();
    let runtime = LeanRuntime::init().expect("Lean runtime initialisation must succeed");
    let host = LeanHost::from_lake_project(runtime, root).expect("host opens cleanly");
    let caps = fixed_fixture::capabilities(&host).expect("load caps");
    let mut session = attempt_session(&caps);
    assert_eq!(source, SOURCE);
    let [candidate] = candidates.as_slice() else {
        panic!("expected one fixed candidate")
    };
    assert!(match (&position, candidate.id.as_str(), candidate.text.as_str()) {
        (ProofPositionSelector::Default, "show", "show n + 0 = n")
        | (ProofPositionSelector::Index { index: 2 | 99 }, "exact", "exact Nat.add_zero n") => true,
        (ProofPositionSelector::AfterText { text, occurrence: None }, "skip", "skip") => text == "intro",
        _ => false,
    });
    let request = ProofAttemptRequest {
        source: source.to_owned(),
        edit: ProofEditTarget::Declaration {
            name: "t".to_owned(),
            position,
        },
        candidates,
        budgets: ModuleQueryOutputBudgets::default(),
    };
    let (env, imports) = session.source_context();
    #[allow(unsafe_code, reason = "closed proof-attempt inputs under fixed fixture custody")]
    // SAFETY: the four exact edits above use the fixed prelude theorem, default budgets
    // and options, in the Elaboration session created here from the admitted images.
    let attested = unsafe {
        caps.shim_capability().proof_attempt_scope(
            env,
            imports,
            fixed_fixture::resolution(),
            request,
            LeanElabOptions::new().source_options(),
        )
    }
    .expect("attest fixed attempt");
    session
        .attempt_proof(&attested, None)
        .expect("attempt_proof dispatch succeeds")
}

fn proof_state_at(source: &str, position: ProofPositionSelector) -> ProofStateInfo {
    let root = fixed_fixture::fixture_root();
    let runtime = LeanRuntime::init().expect("Lean runtime initialisation must succeed");
    let host = LeanHost::from_lake_project(runtime, root).expect("host opens cleanly");
    let caps = fixed_fixture::capabilities(&host).expect("load caps");
    let mut session = attempt_session(&caps);
    assert_eq!(source, SOURCE);
    assert!(matches!(
        position,
        ProofPositionSelector::Default | ProofPositionSelector::Index { index: 2 }
    ));
    let selectors = vec![ModuleQuerySelector::ProofStateInDeclaration {
        id: "ctx".to_owned(),
        declaration: "t".to_owned(),
        position,
        locals_raw: false,
    }];
    let (env, imports) = session.source_context();
    let batch = lean_rs::host_bridge::HostModuleBatch {
        source: SOURCE.into(),
        selectors,
        budgets: ModuleQueryOutputBudgets::default(),
    };
    #[allow(unsafe_code, reason = "closed proof-state projections under fixed fixture custody")]
    // SAFETY: both selectors project the fixed theorem above with default options and
    // budgets in this admitted Elaboration environment; no other source is accepted.
    let request = unsafe {
        caps.shim_capability().module_batch_scope(
            env,
            imports,
            fixed_fixture::resolution(),
            batch,
            LeanElabOptions::new().source_options(),
        )
    }
    .expect("attest fixed query");
    let outcome = session
        .process_module_query_batch(&request, None)
        .expect("module query batch dispatch succeeds");
    let ModuleQueryBatchOutcome::Ok { result, .. } = outcome else {
        panic!("expected Ok batch outcome, got {outcome:?}");
    };
    let item = result
        .items
        .iter()
        .find(|item| item.id() == "ctx")
        .expect("batch carries the ctx selector result");
    let ModuleQueryBatchItem::Ok { result, .. } = item else {
        panic!("expected Ok selector item, got {item:?}");
    };
    let ModuleQueryBatchResult::ProofState(ProofStateResult::State(info)) = result.as_ref() else {
        panic!("expected proof state result, got {result:?}");
    };
    *info.clone()
}

#[test]
fn attempt_envelope_entry_state_matches_proof_state_at_default_position() {
    let outcome = attempt(
        SOURCE,
        ProofPositionSelector::Default,
        vec![ProofCandidate {
            id: "show".to_owned(),
            text: "show n + 0 = n".to_owned(),
        }],
    );
    let ProofAttemptOutcome::Ok { result, .. } = outcome else {
        panic!("expected Ok proof attempt, got {outcome:?}");
    };
    let context = proof_state_at(SOURCE, ProofPositionSelector::Default);
    assert_eq!(result.candidates.len(), 1);
    assert!(
        !result.entry_goals.is_empty(),
        "resolved attempt must render entry goals once per envelope"
    );
    assert_eq!(
        result
            .entry_goals
            .iter()
            .map(|goal| goal.value.as_str())
            .collect::<Vec<_>>(),
        context.goals_before,
        "entry goals must equal the proof-position query's goals_before at the same position"
    );
    assert!(
        result.entry_goals.iter().all(|goal| !goal.truncated),
        "entry goals within budget must not be truncated"
    );
    assert_eq!(
        result.locals, context.locals,
        "entry locals must equal the proof-position query's locals at the same position"
    );
    assert!(
        result.entry_goals[0].value.contains('∀'),
        "default position enters before the first tactic, so the pristine goal keeps the binder: {:?}",
        result.entry_goals[0].value
    );
    assert!(
        !result.candidates[0].goals.is_empty(),
        "candidate spliced after `intro n` must still report its own post-candidate goals"
    );
}

#[test]
fn attempt_envelope_entry_state_matches_proof_state_at_indexed_position() {
    let outcome = attempt(
        SOURCE,
        ProofPositionSelector::Index { index: 2 },
        vec![ProofCandidate {
            id: "exact".to_owned(),
            text: "exact Nat.add_zero n".to_owned(),
        }],
    );
    let ProofAttemptOutcome::Ok { result, .. } = outcome else {
        panic!("expected Ok proof attempt, got {outcome:?}");
    };
    let context = proof_state_at(SOURCE, ProofPositionSelector::Index { index: 2 });
    assert_eq!(result.candidates.len(), 1);
    assert_eq!(
        result.candidates[0].status,
        lean_rs_host::ProofAttemptStatus::Closed,
        "candidate spliced after `show n + 0 = n` closes the goal"
    );
    assert_eq!(
        result
            .entry_goals
            .iter()
            .map(|goal| goal.value.as_str())
            .collect::<Vec<_>>(),
        context.goals_before,
        "entry goals must equal the proof-position query's goals_before at the same position"
    );
    assert_eq!(
        result.locals, context.locals,
        "entry locals must equal the proof-position query's locals at the same position"
    );
    assert!(
        result.locals.iter().any(|local| local.name == "n"),
        "position after `intro n` must report `n` among the entry locals: {:?}",
        result.locals
    );
}

#[test]
fn attempt_envelope_bad_selector_yields_empty_entry_state_and_failed_candidates() {
    let outcome = attempt(
        SOURCE,
        ProofPositionSelector::Index { index: 99 },
        vec![ProofCandidate {
            id: "exact".to_owned(),
            text: "exact Nat.add_zero n".to_owned(),
        }],
    );
    let ProofAttemptOutcome::Ok { result, .. } = outcome else {
        panic!("expected Ok proof attempt, got {outcome:?}");
    };
    assert!(
        result.entry_goals.is_empty() && result.locals.is_empty(),
        "resolution failure must yield empty entry goals and locals"
    );
    assert_eq!(result.candidates.len(), 1);
    assert_eq!(
        result.candidates[0].status,
        lean_rs_host::ProofAttemptStatus::Failed,
        "resolution failure keeps the existing all-failed candidate rows"
    );
}

#[test]
fn attempt_envelope_source_text_fallback_yields_empty_entry_state() {
    // `intro` does not exactly match the tactic `intro n`, so resolution falls
    // back to the source-text path, which carries no elaborated tactic state:
    // the entry fields come out empty while the attempt itself still runs.
    let outcome = attempt(
        SOURCE,
        ProofPositionSelector::AfterText {
            text: "intro".to_owned(),
            occurrence: None,
        },
        vec![ProofCandidate {
            id: "skip".to_owned(),
            text: "skip".to_owned(),
        }],
    );
    let ProofAttemptOutcome::Ok { result, .. } = outcome else {
        panic!("expected Ok proof attempt, got {outcome:?}");
    };
    assert!(
        result.entry_goals.is_empty() && result.locals.is_empty(),
        "source-text fallback carries no tactic state, so entry fields must be empty"
    );
    assert_eq!(
        result.candidates.len(),
        1,
        "unresolvable entry state must not block the attempt itself"
    );
}
