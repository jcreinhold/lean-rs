//! End-to-end tests for the `LeanHost` / `LeanCapabilities` /
//! `LeanSession` cascade.
//!
//! Each test bootstraps the runtime, opens the fixture Lake project,
//! loads the `LeanRsFixture` capability dylib (which pre-resolves
//! thirteen mandatory session symbols — seven environment queries plus
//! the prompt-15 `elaborate` and `kernel_check` pair plus the
//! prompt-17 `check_evidence` and `evidence_summary` pair — and the
//! four optional meta-service symbols), starts a session
//! over an import list, and exercises the typed query methods.

#![allow(clippy::expect_used, clippy::panic)]

use std::path::PathBuf;
use std::time::Instant;

use crate::host::meta::{
    LeanMetaOptions, LeanMetaResponse, LeanMetaService, LeanMetaTransparency, MetaCallStatus, heartbeat_burn,
    infer_type, is_def_eq, whnf,
};
use crate::{
    EvidenceStatus, LEAN_DIAGNOSTIC_BYTE_LIMIT_DEFAULT, LEAN_PROOF_SUMMARY_BYTE_LIMIT, LeanCancellationToken,
    LeanElabOptions, LeanHost, LeanKernelOutcome, LeanSession, LeanSeverity,
};
use lean_rs::LeanRuntime;
use lean_rs::error::{HostStage, LeanError};

// -- fixture setup -------------------------------------------------------

fn fixture_lake_root() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace = manifest_dir
        .parent()
        .and_then(std::path::Path::parent)
        .expect("crates/<name>/ lives two directories beneath the workspace root");
    workspace.join("fixtures").join("lean")
}

fn runtime() -> &'static LeanRuntime {
    LeanRuntime::init().expect("Lean runtime initialisation must succeed")
}

fn fixture_host() -> LeanHost<'static> {
    LeanHost::from_lake_project(runtime(), fixture_lake_root()).expect("host opens cleanly")
}

// -- from_lake_project ---------------------------------------------------

#[test]
fn from_lake_project_missing_path_is_load_error() {
    let err = LeanHost::from_lake_project(runtime(), "/does/not/exist/lean-rs-fixture")
        .expect_err("opening a nonexistent project root must fail");
    match err {
        LeanError::Host(failure) => {
            assert_eq!(failure.stage(), HostStage::Load);
            assert!(
                failure.message().contains("lean-rs-fixture"),
                "diagnostic must name the requested path, got: {:?}",
                failure.message(),
            );
        }
        LeanError::LeanException(exc) => panic!("expected Host(Load) failure, got LeanException {exc:?}"),
        LeanError::Cancelled(cancelled) => panic!("expected Host(Load) failure, got cancellation {cancelled:?}"),
        _ => panic!("expected Host(Load) failure, got future LeanError variant"),
    }
}

// -- load_capabilities ---------------------------------------------------

#[test]
fn load_capabilities_resolves_all_session_symbols() {
    let host = fixture_host();
    let caps = host
        .load_capabilities("lean_rs_fixture", "LeanRsFixture")
        .expect("capability dylib loads + symbols resolve");
    // Sanity: caps is move-constructed, no public observable state to
    // assert against. The follow-on tests prove the cached addresses
    // actually dispatch correctly.
    drop(caps);
}

#[test]
fn load_capabilities_missing_dylib_is_load_error() {
    let host = fixture_host();
    let err = host
        .load_capabilities("does_not_exist", "NoSuchLib")
        .expect_err("missing dylib must fail");
    match err {
        LeanError::Host(failure) => {
            assert_eq!(failure.stage(), HostStage::Load);
            assert!(
                failure.message().contains("NoSuchLib"),
                "diagnostic must name the requested library, got: {:?}",
                failure.message(),
            );
        }
        LeanError::LeanException(exc) => panic!("expected Host(Load) failure, got LeanException {exc:?}"),
        LeanError::Cancelled(cancelled) => panic!("expected Host(Load) failure, got cancellation {cancelled:?}"),
        _ => panic!("expected Host(Load) failure, got future LeanError variant"),
    }
}

// -- session import + query ---------------------------------------------

fn session_over_handles<'lean, 'c>(caps: &'c crate::LeanCapabilities<'lean, 'c>) -> LeanSession<'lean, 'c> {
    caps.session(&["LeanRsFixture.Handles"], None)
        .expect("session imports cleanly")
}

#[test]
fn session_import_then_query_fixture_definition() {
    let host = fixture_host();
    let caps = host
        .load_capabilities("lean_rs_fixture", "LeanRsFixture")
        .expect("load caps");
    let mut session = session_over_handles(&caps);

    // `LeanRsFixture.Handles.nameAnonymous` is the first fixture export
    // in Handles.lean and is reachable through the imported environment.
    let decl = session
        .query_declaration("LeanRsFixture.Handles.nameAnonymous", None)
        .expect("query existing fixture declaration");
    // Returned LeanDeclaration is opaque; the test passes if no error
    // surfaced. Render-checks happen via declaration_name.
    drop(decl);
}

#[test]
fn session_declaration_kind_discriminates() {
    let host = fixture_host();
    let caps = host
        .load_capabilities("lean_rs_fixture", "LeanRsFixture")
        .expect("load caps");
    let mut session = session_over_handles(&caps);

    let fixture_def_kind = session
        .declaration_kind("LeanRsFixture.Handles.nameAnonymous", None)
        .expect("kind for fixture def");
    assert_eq!(
        fixture_def_kind, "definition",
        "fixture `def` must classify as definition"
    );

    let nat_kind = session.declaration_kind("Nat", None).expect("kind for Nat");
    assert_eq!(nat_kind, "inductive", "prelude `Nat` must classify as inductive");

    let missing_kind = session
        .declaration_kind("This.Name.Does.Not.Exist", None)
        .expect("kind query for absent name");
    assert_eq!(missing_kind, "missing", "absent name must classify as missing");
}

#[test]
fn session_declaration_type_round_trips_as_expr() {
    let host = fixture_host();
    let caps = host
        .load_capabilities("lean_rs_fixture", "LeanRsFixture")
        .expect("load caps");
    let mut session = session_over_handles(&caps);

    let type_handle = session
        .declaration_type("LeanRsFixture.Handles.nameAnonymous", None)
        .expect("type query for fixture def")
        .expect("fixture def has a type");
    // Returned LeanExpr is opaque; passing it through any of the
    // prompt-13 fixture exports that accept LeanExpr would prove
    // structural soundness. Here we just confirm the handle exists and
    // drops without panic.
    drop(type_handle);
}

#[test]
fn session_declaration_type_returns_none_for_missing() {
    let host = fixture_host();
    let caps = host
        .load_capabilities("lean_rs_fixture", "LeanRsFixture")
        .expect("load caps");
    let mut session = session_over_handles(&caps);

    let absent = session
        .declaration_type("This.Name.Does.Not.Exist", None)
        .expect("type query for absent name");
    assert!(absent.is_none(), "missing name must yield None");
}

#[test]
fn session_declaration_name_renders_dotted_form() {
    let host = fixture_host();
    let caps = host
        .load_capabilities("lean_rs_fixture", "LeanRsFixture")
        .expect("load caps");
    let mut session = session_over_handles(&caps);

    let rendered = session
        .declaration_name("LeanRsFixture.Handles.nameAnonymous", None)
        .expect("render name");
    assert!(
        rendered.contains("nameAnonymous"),
        "rendered name must contain the leaf component, got {rendered:?}",
    );
}

#[test]
fn session_query_missing_declaration_is_host_error() {
    let host = fixture_host();
    let caps = host
        .load_capabilities("lean_rs_fixture", "LeanRsFixture")
        .expect("load caps");
    let mut session = session_over_handles(&caps);

    let err = session
        .query_declaration("This.Name.Does.Not.Exist", None)
        .expect_err("missing declaration must surface a host error");
    match err {
        LeanError::Host(failure) => {
            assert_eq!(failure.stage(), HostStage::Conversion);
            assert!(
                failure.message().contains("This.Name.Does.Not.Exist"),
                "diagnostic must name the missing declaration, got: {:?}",
                failure.message(),
            );
        }
        LeanError::LeanException(exc) => panic!("expected Host(Conversion) failure, got LeanException {exc:?}"),
        LeanError::Cancelled(cancelled) => panic!("expected Host(Conversion) failure, got cancellation {cancelled:?}"),
        _ => panic!("expected Host(Conversion) failure, got future LeanError variant"),
    }
}

#[test]
fn session_list_declarations_includes_prelude_and_fixture() {
    let host = fixture_host();
    let caps = host
        .load_capabilities("lean_rs_fixture", "LeanRsFixture")
        .expect("load caps");
    let mut session = session_over_handles(&caps);

    let names = session.list_declarations(None).expect("list declarations");
    // The Lean prelude alone contributes thousands; the fixture import
    // is a thin slice on top. Just assert the result is non-empty.
    assert!(
        !names.is_empty(),
        "imported environment must contain at least one declaration"
    );
}

// -- elaborate + kernel_check (prompt 15) -------------------------------

fn session_over_elaboration<'lean, 'c>(caps: &'c crate::LeanCapabilities<'lean, 'c>) -> LeanSession<'lean, 'c> {
    caps.session(&["LeanRsHostShims.Elaboration"], None)
        .expect("session imports cleanly")
}

#[test]
fn elaborate_success_returns_expr() {
    let host = fixture_host();
    let caps = host
        .load_capabilities("lean_rs_fixture", "LeanRsFixture")
        .expect("load caps");
    let mut session = session_over_elaboration(&caps);

    let outcome = session
        .elaborate("(1 + 2 : Nat)", None, &LeanElabOptions::new(), None)
        .expect("host stack reports no exception");
    let expr = outcome.expect("elaboration succeeds for a well-typed Nat term");
    // Returned LeanExpr is opaque; success path is asserted by Ok.
    drop(expr);
}

#[test]
fn elaborate_syntax_failure_reports_diagnostic() {
    let host = fixture_host();
    let caps = host
        .load_capabilities("lean_rs_fixture", "LeanRsFixture")
        .expect("load caps");
    let mut session = session_over_elaboration(&caps);

    let outcome = session
        .elaborate("1 +", None, &LeanElabOptions::new(), None)
        .expect("host stack reports no exception");
    let failure = outcome.expect_err("trailing operator must fail to parse");
    let first = failure
        .diagnostics()
        .first()
        .expect("parse failure must report at least one diagnostic");
    assert_eq!(
        first.severity(),
        LeanSeverity::Error,
        "parse failure diagnostic must be error-severity"
    );
}

#[test]
fn elaborate_type_failure_reports_position() {
    let host = fixture_host();
    let caps = host
        .load_capabilities("lean_rs_fixture", "LeanRsFixture")
        .expect("load caps");
    let mut session = session_over_elaboration(&caps);

    // Mixing `String` with arithmetic against `Nat` triggers an
    // elaborator type error that carries a position.
    let outcome = session
        .elaborate("(1 + \"hi\" : Nat)", None, &LeanElabOptions::new(), None)
        .expect("host stack reports no exception");
    let failure = outcome.expect_err("type-mismatched term must fail to elaborate");
    let diag = failure
        .diagnostics()
        .first()
        .expect("type failure must report at least one diagnostic");
    assert_eq!(
        diag.severity(),
        LeanSeverity::Error,
        "first diagnostic must be error-severity"
    );
    let pos = diag.position().expect("elaborator attached a position");
    assert!(
        pos.line() >= 1 && pos.column() >= 1,
        "position is 1-indexed: line={} column={}",
        pos.line(),
        pos.column(),
    );
    assert!(
        diag.message().len() <= LEAN_DIAGNOSTIC_BYTE_LIMIT_DEFAULT,
        "single diagnostic must fit the per-message byte bound"
    );
}

#[test]
fn kernel_check_small_theorem_returns_evidence() {
    let host = fixture_host();
    let caps = host
        .load_capabilities("lean_rs_fixture", "LeanRsFixture")
        .expect("load caps");
    let mut session = session_over_elaboration(&caps);

    let src = "theorem lean_rs_smoke : 1 + 1 = 2 := rfl";
    let outcome = session
        .kernel_check(src, &LeanElabOptions::new(), None)
        .expect("host stack reports no exception");
    assert_eq!(
        outcome.status(),
        EvidenceStatus::Checked,
        "well-typed theorem must classify as Checked, got {outcome:?}"
    );
    match outcome {
        LeanKernelOutcome::Checked(evidence) => {
            let _cloned = evidence.clone();
            drop(evidence);
        }
        LeanKernelOutcome::Rejected(_) | LeanKernelOutcome::Unavailable(_) | LeanKernelOutcome::Unsupported(_) => {
            panic!("expected Checked variant");
        }
    }
}

#[test]
fn kernel_check_rejects_bad_proof() {
    let host = fixture_host();
    let caps = host
        .load_capabilities("lean_rs_fixture", "LeanRsFixture")
        .expect("load caps");
    let mut session = session_over_elaboration(&caps);

    let src = "theorem lean_rs_bad : 1 = 2 := rfl";
    let outcome = session
        .kernel_check(src, &LeanElabOptions::new(), None)
        .expect("host stack reports no exception");
    assert_eq!(
        outcome.status(),
        EvidenceStatus::Rejected,
        "kernel must reject a false proof, got {outcome:?}"
    );
    match outcome {
        LeanKernelOutcome::Rejected(failure) => {
            assert!(
                !failure.diagnostics().is_empty(),
                "rejected proof must carry at least one diagnostic"
            );
        }
        LeanKernelOutcome::Checked(_) | LeanKernelOutcome::Unavailable(_) | LeanKernelOutcome::Unsupported(_) => {
            panic!("expected Rejected variant");
        }
    }
}

#[test]
fn kernel_check_classifies_unavailable_or_rejected_on_pathological_input() {
    // `Lean.Elab.Frontend.process` is robust: nearly every malformed
    // source produces error diagnostics in the `MessageLog` (the
    // shim's `Rejected` path), not an `IO`-level exception (the
    // shim's `Unavailable` path). The Unavailable branch fires only
    // when `process` itself raises through `IO` — for example on
    // resource exhaustion, internal panic, or runtime failure during
    // task scheduling. Driving any of those from user input alone is
    // not contract: a given Lean release can move the boundary
    // between which inputs surface as diagnostics versus exceptions.
    //
    // This test pins what the Rust mapping *guarantees*: a deeply
    // pathological input must classify as either `Rejected` or
    // `Unavailable` — never `Checked` and never `Unsupported` (those
    // would mean the shim treated broken input as a valid command).
    // It also confirms the four-tag `EvidenceStatus` discriminator is
    // wired correctly for the two failure branches the shim can pick
    // here. The Lean-side classification logic (which decides between
    // `Rejected` and `Unavailable`) is exercised by the fixture's own
    // tests, not by this Rust integration suite.
    let host = fixture_host();
    let caps = host
        .load_capabilities("lean_rs_fixture", "LeanRsFixture")
        .expect("load caps");
    let mut session = session_over_elaboration(&caps);

    let outcome = session
        .kernel_check("theorem :=", &LeanElabOptions::new(), None)
        .expect("host stack reports no exception");
    assert!(
        matches!(outcome.status(), EvidenceStatus::Rejected | EvidenceStatus::Unavailable),
        "malformed source must classify as Rejected or Unavailable, got {outcome:?}"
    );
    match outcome {
        LeanKernelOutcome::Rejected(failure) | LeanKernelOutcome::Unavailable(failure) => {
            assert!(
                !failure.diagnostics().is_empty(),
                "failure outcome must carry at least one diagnostic"
            );
        }
        LeanKernelOutcome::Checked(_) | LeanKernelOutcome::Unsupported(_) => {
            panic!("malformed source must not classify as Checked or Unsupported");
        }
    }
}

#[test]
fn kernel_check_unsupported_on_non_declaration() {
    let host = fixture_host();
    let caps = host
        .load_capabilities("lean_rs_fixture", "LeanRsFixture")
        .expect("load caps");
    let mut session = session_over_elaboration(&caps);

    // `#check` is a command that elaborates cleanly but adds no
    // constant to the environment, so the classifier returns
    // `Unsupported` (no new theorem/definition).
    let outcome = session
        .kernel_check("#check Nat", &LeanElabOptions::new(), None)
        .expect("host stack reports no exception");
    assert_eq!(
        outcome.status(),
        EvidenceStatus::Unsupported,
        "non-declaration command must classify as Unsupported, got {outcome:?}"
    );
}

#[test]
fn check_evidence_revalidates_checked_evidence() {
    let host = fixture_host();
    let caps = host
        .load_capabilities("lean_rs_fixture", "LeanRsFixture")
        .expect("load caps");
    let mut session = session_over_elaboration(&caps);

    let outcome = session
        .kernel_check(
            "theorem lean_rs_recheck : 1 + 1 = 2 := rfl",
            &LeanElabOptions::new(),
            None,
        )
        .expect("host stack reports no exception");
    let evidence = match outcome {
        LeanKernelOutcome::Checked(evidence) => evidence,
        LeanKernelOutcome::Rejected(_) | LeanKernelOutcome::Unavailable(_) | LeanKernelOutcome::Unsupported(_) => {
            panic!("expected Checked variant");
        }
    };

    // Round-trip the cloned handle: re-validation must read the
    // bumped refcount cleanly.
    let cloned = evidence.clone();
    let status = session
        .check_evidence(&cloned, None)
        .expect("re-validation routes through the host stack cleanly");
    assert_eq!(
        status,
        EvidenceStatus::Checked,
        "re-validating a fresh evidence handle against the same environment must succeed"
    );

    // Original handle also re-validates; addDecl does not consume it.
    let status_again = session
        .check_evidence(&evidence, None)
        .expect("re-validation is idempotent");
    assert_eq!(
        status_again,
        EvidenceStatus::Checked,
        "re-validation is idempotent against an unchanged environment"
    );
}

#[test]
fn summarize_evidence_exposes_declaration_name() {
    let host = fixture_host();
    let caps = host
        .load_capabilities("lean_rs_fixture", "LeanRsFixture")
        .expect("load caps");
    let mut session = session_over_elaboration(&caps);

    let outcome = session
        .kernel_check(
            "theorem lean_rs_summary : 1 + 1 = 2 := rfl",
            &LeanElabOptions::new(),
            None,
        )
        .expect("host stack reports no exception");
    let evidence = match outcome {
        LeanKernelOutcome::Checked(evidence) => evidence,
        LeanKernelOutcome::Rejected(_) | LeanKernelOutcome::Unavailable(_) | LeanKernelOutcome::Unsupported(_) => {
            panic!("expected Checked variant");
        }
    };

    let summary = session
        .summarize_evidence(&evidence, None)
        .expect("summary routes through the host stack cleanly");
    assert_eq!(
        summary.declaration_name(),
        "lean_rs_summary",
        "summary must expose the declared name verbatim"
    );
    assert_eq!(summary.kind(), "theorem", "summary must classify the kind as `theorem`");
    let signature = summary.type_signature();
    // The Lean fixture renders types via the default `ToString Expr`
    // instance, which emits the elaborated `Eq.{...} Nat ...` form
    // rather than the surface `=` notation. Either spelling proves the
    // proposition crossed the boundary as text; check for both so the
    // assertion survives a future switch to a pretty-printer.
    assert!(
        signature.contains("Eq") || signature.contains('='),
        "type signature must mention equality on the proposition, got: {signature:?}",
    );
    assert!(
        signature.contains("Nat"),
        "type signature must mention the underlying `Nat` carrier, got: {signature:?}",
    );
    assert!(
        !signature.contains("rfl"),
        "type signature must not leak the proof term `rfl`, got: {signature:?}",
    );
    for field in [summary.declaration_name(), summary.kind(), summary.type_signature()] {
        assert!(
            field.len() <= LEAN_PROOF_SUMMARY_BYTE_LIMIT,
            "ProofSummary field exceeds the documented byte bound: {} bytes",
            field.len()
        );
    }
}

#[test]
fn diagnostic_byte_limit_truncates() {
    let host = fixture_host();
    let caps = host
        .load_capabilities("lean_rs_fixture", "LeanRsFixture")
        .expect("load caps");
    let mut session = session_over_elaboration(&caps);

    // Multiple unbound identifiers produce multiple diagnostics; a
    // single-byte budget cannot fit them all and must be reported as
    // truncated.
    let opts = LeanElabOptions::new().diagnostic_byte_limit(1);
    let outcome = session
        .elaborate("(foo + bar + baz : Nat)", None, &opts, None)
        .expect("host stack reports no exception");
    let failure = outcome.expect_err("unbound identifiers must fail to elaborate");
    assert!(
        failure.truncated(),
        "tiny diagnostic budget must surface as truncated; diagnostics returned = {}",
        failure.diagnostics().len(),
    );
}

// -- timing note: amortised import across many queries -------------------
//
// Informational only. Per the prompt's "no performance claim without
// numbers" rule, this test prints the numbers and does not assert
// thresholds. Run with `cargo test session_reuse_amortises_import -- --nocapture`.

#[test]
fn session_reuse_amortises_import() {
    // Re-importing the Lean prelude is multi-second per call; 4 queries
    // is plenty to make the amortisation observable without dragging
    // the suite into the multi-minute range.
    const QUERIES: usize = 4;
    let host = fixture_host();
    let caps = host
        .load_capabilities("lean_rs_fixture", "LeanRsFixture")
        .expect("load caps");

    // (a) One session, many queries.
    let start_reuse = Instant::now();
    {
        let mut session = caps
            .session(&["LeanRsFixture.Handles"], None)
            .expect("session imports cleanly");
        for _ in 0..QUERIES {
            let kind = session
                .declaration_kind("LeanRsFixture.Handles.nameAnonymous", None)
                .expect("query");
            assert_eq!(kind, "definition");
        }
    }
    let reuse_elapsed = start_reuse.elapsed();

    // (b) Fresh session per query.
    let start_per_query = Instant::now();
    for _ in 0..QUERIES {
        let mut session = caps
            .session(&["LeanRsFixture.Handles"], None)
            .expect("session imports cleanly");
        let kind = session
            .declaration_kind("LeanRsFixture.Handles.nameAnonymous", None)
            .expect("query");
        assert_eq!(kind, "definition");
    }
    let per_query_elapsed = start_per_query.elapsed();

    println!(
        "session_reuse_amortises_import: \
         {QUERIES} queries reusing one session took {reuse_elapsed:?}; \
         re-importing per query took {per_query_elapsed:?}",
    );
    // Sanity floor: per-query reimporting cannot be faster than reuse
    // (importing is the dominant cost). If this ever inverts, something
    // is wrong with the cached-symbol path.
    assert!(
        per_query_elapsed >= reuse_elapsed,
        "per-query reimport ({per_query_elapsed:?}) must not beat session reuse ({reuse_elapsed:?})",
    );
}

// -- run_meta (prompt 16) -----------------------------------------------
//
// Each test imports `LeanRsHostShims.Meta` (which also pulls in
// `LeanRsHostShims.Elaboration` via the dependency edge). The fixture
// dylib exports the four optional meta-service symbols, so the
// `SessionSymbols::resolve` tolerant lookup finds them and `run_meta`
// dispatches through cached addresses.

fn session_over_meta<'lean, 'c>(caps: &'c crate::LeanCapabilities<'lean, 'c>) -> LeanSession<'lean, 'c> {
    caps.session(&["LeanRsFixture.Meta", "LeanRsHostShims.Meta"], None)
        .expect("session imports cleanly")
}

fn meta_expr<'lean>(session: &mut LeanSession<'lean, '_>, symbol: &str) -> lean_rs::LeanExpr<'lean> {
    session
        .call_capability::<((),), lean_rs::LeanExpr<'lean>>(symbol, ((),), None)
        .expect("fixture expression export dispatches cleanly")
}

fn assert_is_def_eq_response(response: &LeanMetaResponse<bool>, expected: bool) {
    assert_eq!(
        response.status(),
        MetaCallStatus::Ok,
        "isDefEq must return Ok({expected}), got {response:?}",
    );
    match response {
        LeanMetaResponse::Ok(actual) => assert_eq!(*actual, expected),
        LeanMetaResponse::Failed(_) | LeanMetaResponse::TimeoutOrHeartbeat(_) | LeanMetaResponse::Unsupported(_) => {
            panic!("expected Ok({expected}) variant");
        }
    }
}

#[test]
fn meta_registry_exposes_four_pinned_services() {
    let services = [
        infer_type().name(),
        whnf().name(),
        heartbeat_burn().name(),
        is_def_eq().name(),
    ];
    assert_eq!(
        services,
        [
            "lean_rs_host_meta_infer_type",
            "lean_rs_host_meta_whnf",
            "lean_rs_host_meta_heartbeat_burn",
            "lean_rs_host_meta_is_def_eq",
        ],
    );
    assert_eq!(
        is_def_eq().required_imports(),
        ["LeanRsHostShims.Meta"],
        "new service must use the existing meta shim module",
    );
}

#[test]
fn meta_infer_type_returns_ok_for_nat_type() {
    let host = fixture_host();
    let caps = host
        .load_capabilities("lean_rs_fixture", "LeanRsFixture")
        .expect("load caps");
    let mut session = session_over_meta(&caps);

    // The type of `Nat.zero` is `Nat`; inferring its type yields `Type`.
    // Using a Lean-produced Expr keeps the test honest — Rust never
    // constructs an Expr directly.
    let expr = session
        .declaration_type("Nat.zero", None)
        .expect("type query for Nat.zero")
        .expect("Nat.zero has a type");
    let outcome = session
        .run_meta(&infer_type(), expr, &LeanMetaOptions::new(), None)
        .expect("host stack reports no exception");
    assert_eq!(
        outcome.status(),
        MetaCallStatus::Ok,
        "Meta.inferType on `Nat` must succeed, got {outcome:?}",
    );
    match outcome {
        LeanMetaResponse::Ok(payload) => {
            // Opaque LeanExpr; the success path is asserted by status().
            drop(payload);
        }
        LeanMetaResponse::Failed(_) | LeanMetaResponse::TimeoutOrHeartbeat(_) | LeanMetaResponse::Unsupported(_) => {
            panic!("expected Ok variant");
        }
    }
}

#[test]
fn meta_whnf_returns_ok_for_nat_type() {
    let host = fixture_host();
    let caps = host
        .load_capabilities("lean_rs_fixture", "LeanRsFixture")
        .expect("load caps");
    let mut session = session_over_meta(&caps);

    let expr = session
        .declaration_type("Nat.zero", None)
        .expect("type query for Nat.zero")
        .expect("Nat.zero has a type");
    let outcome = session
        .run_meta(&whnf(), expr, &LeanMetaOptions::new(), None)
        .expect("host stack reports no exception");
    assert_eq!(
        outcome.status(),
        MetaCallStatus::Ok,
        "Meta.whnf on a constant Expr must succeed, got {outcome:?}",
    );
    match outcome {
        LeanMetaResponse::Ok(payload) => drop(payload),
        LeanMetaResponse::Failed(_) | LeanMetaResponse::TimeoutOrHeartbeat(_) | LeanMetaResponse::Unsupported(_) => {
            panic!("expected Ok variant");
        }
    }
}

#[test]
fn meta_heartbeat_burn_yields_timeout_status() {
    // timing note: with heartbeat_limit(1), Core.checkMaxHeartbeats
    // trips on the very first iteration; the test completes in well
    // under a millisecond. No threshold asserted (per the project's
    // "no performance claim without numbers" rule).
    let host = fixture_host();
    let caps = host
        .load_capabilities("lean_rs_fixture", "LeanRsFixture")
        .expect("load caps");
    let mut session = session_over_meta(&caps);

    // Any Expr will do — heartbeat_burn ignores its argument.
    let expr = session
        .declaration_type("Nat.zero", None)
        .expect("type query for Nat.zero")
        .expect("Nat.zero has a type");
    let opts = LeanMetaOptions::new().heartbeat_limit(1);
    let outcome = session
        .run_meta(&heartbeat_burn(), expr, &opts, None)
        .expect("host stack reports no exception");
    assert_eq!(
        outcome.status(),
        MetaCallStatus::TimeoutOrHeartbeat,
        "heartbeat budget = 1 must surface as TimeoutOrHeartbeat, got {outcome:?}",
    );
    match outcome {
        LeanMetaResponse::TimeoutOrHeartbeat(failure) => {
            let first = failure
                .diagnostics()
                .first()
                .expect("heartbeat failure must carry at least one diagnostic");
            assert_eq!(first.severity(), LeanSeverity::Error);
            assert!(
                !first.message().is_empty(),
                "heartbeat diagnostic message must be non-empty",
            );
        }
        LeanMetaResponse::Ok(_) | LeanMetaResponse::Failed(_) | LeanMetaResponse::Unsupported(_) => {
            panic!("expected TimeoutOrHeartbeat variant");
        }
    }
}

#[test]
fn meta_is_def_eq_reducible_alias_matches_nat() {
    let host = fixture_host();
    let caps = host
        .load_capabilities("lean_rs_fixture", "LeanRsFixture")
        .expect("load caps");
    let mut session = session_over_meta(&caps);

    let lhs = meta_expr(&mut session, "lean_rs_fixture_meta_expr_reducible_nat_alias");
    let rhs = meta_expr(&mut session, "lean_rs_fixture_meta_expr_nat");
    let outcome = session
        .run_meta(
            &is_def_eq(),
            (lhs, rhs, LeanMetaTransparency::Reducible),
            &LeanMetaOptions::new(),
            None,
        )
        .expect("host stack reports no exception");
    assert_is_def_eq_response(&outcome, true);
}

#[test]
fn meta_is_def_eq_distinguishes_nat_and_bool() {
    let host = fixture_host();
    let caps = host
        .load_capabilities("lean_rs_fixture", "LeanRsFixture")
        .expect("load caps");
    let mut session = session_over_meta(&caps);

    let lhs = meta_expr(&mut session, "lean_rs_fixture_meta_expr_nat");
    let rhs = meta_expr(&mut session, "lean_rs_fixture_meta_expr_bool");
    let outcome = session
        .run_meta(
            &is_def_eq(),
            (lhs, rhs, LeanMetaTransparency::Reducible),
            &LeanMetaOptions::new(),
            None,
        )
        .expect("host stack reports no exception");
    assert_is_def_eq_response(&outcome, false);
}

#[test]
fn meta_is_def_eq_default_does_not_unfold_irreducible_alias() {
    let host = fixture_host();
    let caps = host
        .load_capabilities("lean_rs_fixture", "LeanRsFixture")
        .expect("load caps");
    let mut session = session_over_meta(&caps);

    let lhs = meta_expr(&mut session, "lean_rs_fixture_meta_expr_irreducible_nat_alias");
    let rhs = meta_expr(&mut session, "lean_rs_fixture_meta_expr_nat");
    let outcome = session
        .run_meta(
            &is_def_eq(),
            (lhs, rhs, LeanMetaTransparency::Default),
            &LeanMetaOptions::new(),
            None,
        )
        .expect("host stack reports no exception");
    assert_is_def_eq_response(&outcome, false);
}

#[test]
fn meta_is_def_eq_all_unfolds_irreducible_alias() {
    let host = fixture_host();
    let caps = host
        .load_capabilities("lean_rs_fixture", "LeanRsFixture")
        .expect("load caps");
    let mut session = session_over_meta(&caps);

    let lhs = meta_expr(&mut session, "lean_rs_fixture_meta_expr_irreducible_nat_alias");
    let rhs = meta_expr(&mut session, "lean_rs_fixture_meta_expr_nat");
    let outcome = session
        .run_meta(
            &is_def_eq(),
            (lhs, rhs, LeanMetaTransparency::All),
            &LeanMetaOptions::new(),
            None,
        )
        .expect("host stack reports no exception");
    assert_is_def_eq_response(&outcome, true);
}

#[test]
fn meta_is_def_eq_surfaces_heartbeat_exhaustion() {
    let host = fixture_host();
    let caps = host
        .load_capabilities("lean_rs_fixture", "LeanRsFixture")
        .expect("load caps");
    let mut session = session_over_meta(&caps);

    let lhs = meta_expr(&mut session, "lean_rs_fixture_meta_expr_large_nat_left");
    let rhs = meta_expr(&mut session, "lean_rs_fixture_meta_expr_large_nat_right");
    let opts = LeanMetaOptions::new().heartbeat_limit(1);
    let outcome = session
        .run_meta(&is_def_eq(), (lhs, rhs, LeanMetaTransparency::All), &opts, None)
        .expect("host stack reports no exception");
    assert_eq!(
        outcome.status(),
        MetaCallStatus::TimeoutOrHeartbeat,
        "large equality with heartbeat budget 1 must surface TimeoutOrHeartbeat, got {outcome:?}",
    );
}

#[test]
fn meta_is_def_eq_pre_cancelled_token_returns_cancelled() {
    let host = fixture_host();
    let caps = host
        .load_capabilities("lean_rs_fixture", "LeanRsFixture")
        .expect("load caps");
    let mut session = session_over_meta(&caps);

    let lhs = meta_expr(&mut session, "lean_rs_fixture_meta_expr_nat");
    let rhs = meta_expr(&mut session, "lean_rs_fixture_meta_expr_nat");
    let before = session.stats();
    let token = LeanCancellationToken::new();
    token.cancel();
    let err = session
        .run_meta(
            &is_def_eq(),
            (lhs, rhs, LeanMetaTransparency::Reducible),
            &LeanMetaOptions::new(),
            Some(&token),
        )
        .expect_err("pre-cancelled token must return Cancelled");
    match err {
        LeanError::Cancelled(_) => {}
        LeanError::LeanException(exc) => panic!("expected Cancelled, got LeanException {exc:?}"),
        LeanError::Host(failure) => panic!("expected Cancelled, got Host {failure:?}"),
        _ => panic!("expected Cancelled, got future LeanError variant"),
    }
    assert_eq!(
        session.stats().ffi_calls,
        before.ffi_calls,
        "pre-cancelled run_meta must not enter another FFI call",
    );
}

#[test]
fn meta_missing_optional_symbol_returns_unsupported() {
    let host = fixture_host();
    let caps = host
        .load_capabilities("lean_rs_fixture", "LeanRsFixture")
        .expect("load caps");
    let mut session = session_over_meta(&caps);

    let expr = meta_expr(&mut session, "lean_rs_fixture_meta_expr_nat");
    let missing: LeanMetaService<lean_rs::LeanExpr<'_>, lean_rs::LeanExpr<'_>> =
        LeanMetaService::new("lean_rs_host_meta_missing_for_test", &["LeanRsHostShims.Meta"]);
    let outcome = session
        .run_meta(&missing, expr, &LeanMetaOptions::new(), None)
        .expect("missing optional service is classified, not a load failure");
    assert_eq!(
        outcome.status(),
        MetaCallStatus::Unsupported,
        "missing optional meta symbol must return Unsupported, got {outcome:?}",
    );
}
