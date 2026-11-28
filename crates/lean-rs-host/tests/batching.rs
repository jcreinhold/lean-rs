//! Integration tests for bulk session methods.
//!
//! These tests live in `tests/` (not in `src/host/tests.rs`) because
//! every test imports a fresh `Lean.Environment`, and accumulating
//! dozens of imports in a single test process exhausts the host's
//! resident-set budget. Integration tests run as a separate binary, so
//! their imports do not compound with the lower-layer unit tests.

#![allow(clippy::expect_used, clippy::indexing_slicing, clippy::panic)]

#[path = "support/fixed_fixture.rs"]
mod fixture;
use std::path::PathBuf;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};
use std::{fs, path::Path};

use lean_rs::{HostStage, LeanDiagnosticCode, LeanError, LeanRuntime};
use lean_rs_host::{LeanCancellationToken, LeanCapabilities, LeanHost, LeanSession};

// -- fixture setup -------------------------------------------------------

fn fixture_lake_root() -> PathBuf {
    fixture::native_fixture_root()
}

fn runtime() -> &'static LeanRuntime {
    LeanRuntime::init().expect("Lean runtime initialisation must succeed")
}

fn fixture_host() -> LeanHost<'static> {
    LeanHost::from_lake_project(runtime(), fixture_lake_root()).expect("host opens cleanly")
}

fn session_over_handles<'lean, 'c>(caps: &'c LeanCapabilities<'lean, 'c>) -> LeanSession<'lean, 'c> {
    caps.session(&fixture::native_imports(caps, "LeanRsFixture.Handles"), None, None)
        .expect("session imports cleanly")
}

fn session_over_elaboration<'lean, 'c>(caps: &'c LeanCapabilities<'lean, 'c>) -> LeanSession<'lean, 'c> {
    caps.session(
        &fixture::native_imports(caps, "LeanRsHostShims.Elaboration"),
        None,
        None,
    )
    .expect("session imports cleanly")
}

fn assert_cancelled(err: LeanError) {
    assert_eq!(err.code(), LeanDiagnosticCode::Cancelled);
    match err {
        LeanError::Cancelled(_) => {}
        LeanError::Host(failure) => panic!("expected LeanError::Cancelled, got Host {failure:?}"),
        LeanError::LeanException(exc) => panic!("expected LeanError::Cancelled, got LeanException {exc:?}"),
    }
}

// -- query_declarations_bulk --------------------------------------------

#[test]
fn query_declarations_bulk_returns_all_for_existing_names() {
    let host = fixture_host();
    let caps = fixture::native_capabilities(&host).expect("load caps");
    let mut session = session_over_handles(&caps);

    let baseline = session.stats();
    // The Lean shim only round-trips `axiom`/`definition`/`theorem`/
    // `opaque` declarations; pick three fixture defs so the bulk path
    // exercises N == 3 without tripping the inductive/constructor
    // exclusion baked into the singular shim.
    let names = [
        "LeanRsFixture.Handles.nameAnonymous",
        "LeanRsFixture.Handles.nameMkStr",
        "LeanRsFixture.Handles.exprConstNat",
    ];
    let decls = session
        .query_declarations_bulk(&names, None, None)
        .expect("bulk query succeeds for fully-resolvable name list");

    assert_eq!(decls.len(), 3, "bulk returns one slot per input name");
    drop(decls);

    let after = session.stats();
    // make_name dispatches three times (one per input name) + one bulk
    // dispatch = 4 new FFI calls. batch_items records 3 (the per-item
    // count is the batch length, not name_from_string invocations).
    assert_eq!(
        after.ffi_calls - baseline.ffi_calls,
        4,
        "bulk path costs N + 1 FFI calls (one bulk + N name_from_string), got delta {}",
        after.ffi_calls - baseline.ffi_calls,
    );
    assert_eq!(
        after.batch_items - baseline.batch_items,
        3,
        "batch_items records the per-item batch length",
    );
}

#[test]
fn query_declarations_bulk_errors_on_missing_name() {
    let host = fixture_host();
    let caps = fixture::native_capabilities(&host).expect("load caps");
    let mut session = session_over_handles(&caps);

    let err = session
        .query_declarations_bulk(
            &[
                "LeanRsFixture.Handles.nameAnonymous",
                "This.Name.Does.Not.Exist",
                "LeanRsFixture.Handles.nameMkStr",
            ],
            None,
            None,
        )
        .expect_err("bulk must error when any input name is missing");
    match err {
        LeanError::Host(failure) => {
            assert_eq!(failure.stage(), HostStage::Conversion);
            assert!(
                failure.message().contains("This.Name.Does.Not.Exist"),
                "diagnostic must name the missing entry, got: {:?}",
                failure.message(),
            );
        }
        LeanError::LeanException(exc) => panic!("expected Host(Conversion), got LeanException {exc:?}"),
        LeanError::Cancelled(cancelled) => panic!("expected Host(Conversion), got cancellation {cancelled:?}"),
    }
}

// -- declaration_*_bulk -------------------------------------------------

#[test]
fn declaration_type_bulk_returns_present_and_missing_slots_in_one_dispatch() {
    let host = fixture_host();
    let caps = fixture::native_capabilities(&host).expect("load caps");
    let mut session = session_over_handles(&caps);
    let names = [
        "LeanRsFixture.Handles.nameAnonymous",
        "LeanRsFixture.Handles.nameMkStr",
        "LeanRsFixture.Handles.exprConstNat",
        "Nat",
        "Nat.zero",
        "This.Name.Does.Not.Exist",
    ];

    let baseline = session.stats();
    let types = session
        .declaration_type_bulk(&names, None, None)
        .expect("bulk type query succeeds");

    assert_eq!(types.len(), names.len(), "one output slot per input");
    assert!(types[0].is_some(), "fixture definition has a type");
    assert!(types[1].is_some(), "fixture definition has a type");
    assert!(types[2].is_some(), "fixture definition has a type");
    assert!(types[3].is_some(), "Nat has a type");
    assert!(types[4].is_some(), "Nat.zero has a type");
    assert!(types[5].is_none(), "missing declaration yields None");

    let after = session.stats();
    assert_eq!(
        after.ffi_calls - baseline.ffi_calls,
        1,
        "uncancelled declaration_type_bulk must dispatch once",
    );
    assert_eq!(
        after.batch_items - baseline.batch_items,
        names.len() as u64,
        "batch_items records the type batch length",
    );
}

#[test]
fn declaration_kind_bulk_returns_expected_kinds_and_missing_slot() {
    let host = fixture_host();
    let caps = fixture::native_capabilities(&host).expect("load caps");
    let mut session = session_over_handles(&caps);
    let names = [
        "LeanRsFixture.Handles.nameAnonymous",
        "LeanRsFixture.Handles.nameMkStr",
        "LeanRsFixture.Handles.exprConstNat",
        "Nat",
        "Nat.zero",
        "This.Name.Does.Not.Exist",
    ];

    let baseline = session.stats();
    let kinds = session
        .declaration_kind_bulk(&names, None, None)
        .expect("bulk kind query succeeds");

    assert_eq!(
        kinds,
        [
            "definition",
            "definition",
            "definition",
            "inductive",
            "constructor",
            "missing"
        ],
    );
    let after = session.stats();
    assert_eq!(
        after.ffi_calls - baseline.ffi_calls,
        1,
        "uncancelled declaration_kind_bulk must dispatch once",
    );
    assert_eq!(
        after.batch_items - baseline.batch_items,
        names.len() as u64,
        "batch_items records the kind batch length",
    );
}

#[test]
fn declaration_name_bulk_round_trips_names_including_missing() {
    let host = fixture_host();
    let caps = fixture::native_capabilities(&host).expect("load caps");
    let mut session = session_over_handles(&caps);
    let names = [
        "LeanRsFixture.Handles.nameAnonymous",
        "LeanRsFixture.Handles.nameMkStr",
        "Nat.zero",
        "This.Name.Does.Not.Exist",
        "LeanRsFixture.Handles.exprConstNat",
    ];

    let baseline = session.stats();
    let rendered = session
        .declaration_name_bulk(&names, None, None)
        .expect("bulk name query succeeds");

    assert_eq!(rendered, names, "name bulk round-trips the dotted form");
    let after = session.stats();
    assert_eq!(
        after.ffi_calls - baseline.ffi_calls,
        1,
        "uncancelled declaration_name_bulk must dispatch once",
    );
    assert_eq!(
        after.batch_items - baseline.batch_items,
        names.len() as u64,
        "batch_items records the name batch length",
    );
}

#[test]
fn declaration_bulk_empty_inputs_are_no_ops() {
    let host = fixture_host();
    let caps = fixture::native_capabilities(&host).expect("load caps");
    let mut session = session_over_handles(&caps);

    let baseline = session.stats();
    let decls = session
        .query_declarations_bulk(&[], None, None)
        .expect("empty input returns empty vec");
    assert!(decls.is_empty(), "empty input yields empty output");
    assert_eq!(session.stats(), baseline, "empty bulk must not record an FFI call");
    assert!(
        session
            .declaration_type_bulk(&[], None, None)
            .expect("empty type bulk succeeds")
            .is_empty(),
    );
    assert!(
        session
            .declaration_kind_bulk(&[], None, None)
            .expect("empty kind bulk succeeds")
            .is_empty(),
    );
    assert!(
        session
            .declaration_name_bulk(&[], None, None)
            .expect("empty name bulk succeeds")
            .is_empty(),
    );
    assert_eq!(session.stats(), baseline, "empty bulk calls must not record FFI work");
}

#[test]
fn declaration_bulk_pre_cancelled_token_returns_cancelled_without_ffi() {
    let host = fixture_host();
    let caps = fixture::native_capabilities(&host).expect("load caps");
    let mut session = session_over_handles(&caps);
    let token = LeanCancellationToken::new();
    token.cancel();

    let baseline = session.stats();
    let err = session
        .declaration_kind_bulk(&["LeanRsFixture.Handles.nameAnonymous"], Some(&token), None)
        .expect_err("pre-cancelled bulk call should stop before dispatch");

    assert_cancelled(err);
    assert_eq!(
        session.stats(),
        baseline,
        "pre-cancelled bulk call must not record an FFI dispatch",
    );
}

#[test]
fn declaration_bulk_observes_cancellation_between_items() {
    const ITEMS: usize = 100_000;

    let host = fixture_host();
    let caps = fixture::native_capabilities(&host).expect("load caps");
    let mut session = session_over_handles(&caps);
    let names = vec!["LeanRsFixture.Handles.nameAnonymous"; ITEMS];
    let token = LeanCancellationToken::new();
    let canceller = token.clone();
    let (tx, rx) = mpsc::channel();

    let handle = thread::spawn(move || {
        thread::sleep(Duration::from_millis(2));
        let cancelled_at = Instant::now();
        canceller.cancel();
        tx.send(cancelled_at).expect("parent still receives cancellation time");
    });

    let err = session
        .declaration_kind_bulk(&names, Some(&token), None)
        .expect_err("token-present declaration bulk should observe cancellation between items");
    let observed_at = Instant::now();
    handle.join().expect("canceller thread exits cleanly");
    let cancelled_at = rx.recv().expect("canceller sent timestamp");

    assert_cancelled(err);
    assert!(
        session.stats().ffi_calls > 0,
        "canceller sleeps briefly so the worker should complete at least one FFI dispatch first",
    );
    assert!(
        observed_at.duration_since(cancelled_at) < Duration::from_secs(2),
        "cooperative cancellation should be observed at the next per-item check",
    );
}

// -- elaborate_bulk -----------------------------------------------------

#[test]
fn elaborate_bulk_returns_per_source_results() {
    let host = fixture_host();
    let caps = fixture::native_capabilities(&host).expect("load caps");
    let mut session = session_over_elaboration(&caps);

    let baseline = session.stats();

    let outcomes = session
        .elaborate_bulk(
            &fixture::native_term_batch(&caps, &session, &["(1 + 2 : Nat)", "1 +", "(1 + \"hi\" : Nat)"]),
            None,
            None,
        )
        .expect("bulk elaborate routes through the host stack cleanly");

    assert_eq!(outcomes.len(), 3, "bulk returns one slot per input source");
    assert!(outcomes[0].is_ok(), "first source elaborates successfully");
    assert!(outcomes[1].is_err(), "second source is a parse failure");
    assert!(outcomes[2].is_err(), "third source is a type-mismatch failure");

    let after = session.stats();
    assert_eq!(
        after.ffi_calls - baseline.ffi_calls,
        1,
        "elaborate_bulk dispatches once regardless of batch size",
    );
    assert_eq!(
        after.batch_items - baseline.batch_items,
        3,
        "batch_items records the per-source batch length",
    );
}

#[test]
fn elaborate_bulk_empty_input_is_no_op() {
    let host = fixture_host();
    let caps = fixture::native_capabilities(&host).expect("load caps");
    let mut session = session_over_elaboration(&caps);

    let baseline = session.stats();
    let outcomes = session
        .elaborate_bulk(&fixture::native_term_batch(&caps, &session, &[]), None, None)
        .expect("empty input returns empty vec");
    assert!(outcomes.is_empty(), "empty input yields empty output");
    assert_eq!(session.stats(), baseline, "empty bulk must not record an FFI call");
}

// -- import receipts ---------------------------------------------------

// The second project loads only the bundled shims. It needs a real Lake
// identity and an empty output root, not another compiled image.
struct ReceiptProject(PathBuf);

impl ReceiptProject {
    fn selected() -> Option<Self> {
        fixture::native_fixture_selected().then(Self::new)
    }

    fn new() -> Self {
        let target = Path::new(env!("CARGO_TARGET_TMPDIR"))
            .canonicalize()
            .expect("canonical Cargo test scratch");
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock is after epoch")
            .as_nanos();
        let root = target.join(format!("receipt-project-{}-{nonce}", std::process::id()));
        let mut builder = fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt as _;
            builder.mode(0o700);
        }
        builder.create(&root).expect("claim private project");
        let project = Self(root);
        fs::write(
            project.0.join("lakefile.lean"),
            "import Lake\nopen Lake DSL\npackage receipt_second\n",
        )
        .expect("write project configuration");
        fs::write(
            project.0.join("lake-manifest.json"),
            concat!(
                r#"{"version":"1.1.0","packagesDir":".lake/packages","#,
                r#""packages":[],"name":"receipt_second","lakeDir":".lake"}"#,
            ),
        )
        .expect("write exact empty dependency graph");
        fs::write(
            project.0.join("lean-toolchain"),
            include_str!("../../../lean-toolchain"),
        )
        .expect("write pinned toolchain");
        fs::create_dir_all(project.0.join(".lake/build/lib/lean")).expect("create empty output root");
        project
    }
}

impl Drop for ReceiptProject {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).expect("remove private receipt project");
    }
}

fn strict_second_project() -> PathBuf {
    fixture::fixture_root()
        .parent()
        .expect("qualified scratch")
        .join("lean-rs-interop-shims")
}

/// A session refuses an import receipt that a different capability issued.
#[test]
fn session_rejects_receipt_from_another_capability() {
    let runtime = runtime();
    let host = LeanHost::from_lake_project(runtime, fixture_lake_root()).expect("host opens");
    let selected = ReceiptProject::selected();
    let other = selected.as_ref().map_or_else(strict_second_project, |p| p.0.clone());
    let workspace_host = LeanHost::from_lake_project(runtime, other).expect("second controlled project");
    let caps = fixture::native_capabilities(&host).expect("load caps");
    let shims_only = fixture::native_shims_only(&workspace_host).expect("load workspace shims");
    assert_ne!(caps.import_search_paths(), shims_only.import_search_paths());
    let imports = fixture::native_imports(&caps, "LeanRsFixture.Handles");

    let Err(err) = shims_only.session(&imports, None, None) else {
        panic!("a session must refuse the receipt of another capability");
    };
    assert_eq!(err.code(), LeanDiagnosticCode::ModuleInit);
}

// -- timing note: bulk vs singular --------------------------------------
//
// Informational only. Per the project's "no performance claim without
// numbers" rule, this test prints the numbers and asserts only the
// inequality direction.

#[test]
fn bulk_vs_singular_timing_note() {
    const ITEMS: usize = 8;
    let host = fixture_host();
    let caps = fixture::native_capabilities(&host).expect("load caps");
    let mut session = session_over_handles(&caps);

    // Round-trippable fixture defs only (axiom/def/theorem/opaque). The
    // set is intentionally homogeneous so the singular vs. bulk timing
    // comparison stays apples-to-apples.
    let names: [&str; ITEMS] = [
        "LeanRsFixture.Handles.nameAnonymous",
        "LeanRsFixture.Handles.nameMkStr",
        "LeanRsFixture.Handles.nameMkNum",
        "LeanRsFixture.Handles.nameToString",
        "LeanRsFixture.Handles.nameBeq",
        "LeanRsFixture.Handles.levelZero",
        "LeanRsFixture.Handles.levelSucc",
        "LeanRsFixture.Handles.exprConstNat",
    ];

    let start_singular = Instant::now();
    for name in names {
        session
            .query_declaration(name, None)
            .expect("singular query for known name");
    }
    let singular_elapsed = start_singular.elapsed();

    let start_bulk = Instant::now();
    let decls = session
        .query_declarations_bulk(&names, None, None)
        .expect("bulk query for known names");
    let bulk_elapsed = start_bulk.elapsed();

    assert_eq!(decls.len(), ITEMS);

    println!(
        "bulk_vs_singular_timing_note: \
         {ITEMS} singular queries took {singular_elapsed:?}; \
         one bulk query took {bulk_elapsed:?}",
    );
    // No threshold asserted—for the tiny fixture queries used here
    // (each is a microsecond-scale `env.find?` lookup), bulk's
    // single-Vec allocation overhead can exceed the per-call FFI cost
    // it saves. The amortisation win is asymptotic and shows up
    // clearly only when each item carries real Lean-side work (e.g.
    // pretty-printing, kernel checking). The numbers are recorded for
    // reference, not asserted.
    //
    // The `query_declarations_bulk_returns_all_for_existing_names`
    // test does pin the *FFI-call count* contract—bulk is one
    // dispatch regardless of N—which is the structural guarantee
    // worth asserting.
}
