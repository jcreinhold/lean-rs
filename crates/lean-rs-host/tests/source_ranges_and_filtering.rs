//! Integration coverage for declaration source ranges and Lean-side
//! declaration filtering.

#![allow(clippy::expect_used, clippy::indexing_slicing, clippy::panic)]

use std::path::{Path, PathBuf};

use lean_rs::{LeanDiagnosticCode, LeanError, LeanRuntime};
use lean_rs_host::{LeanCancellationToken, LeanCapabilities, LeanDeclarationFilter, LeanHost, LeanSession};

fn fixture_lake_root() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace = manifest_dir
        .parent()
        .and_then(Path::parent)
        .expect("crates/<name>/ lives two directories beneath the workspace root");
    workspace.join("fixtures").join("lean")
}

fn source_fixture_path() -> PathBuf {
    fixture_lake_root().join("LeanRsFixture").join("SourceRanges.lean")
}

fn runtime() -> &'static LeanRuntime {
    LeanRuntime::init().expect("Lean runtime initialisation must succeed")
}

fn fixture_host() -> LeanHost<'static> {
    LeanHost::from_lake_project(runtime(), fixture_lake_root()).expect("host opens cleanly")
}

fn session_over_source_ranges<'lean, 'c>(caps: &'c LeanCapabilities<'lean, 'c>) -> LeanSession<'lean, 'c> {
    caps.session(&["LeanRsFixture.SourceRanges"], None, None)
        .expect("session imports source-range fixture")
}

fn expected_known_theorem_line() -> u32 {
    let source = std::fs::read_to_string(source_fixture_path()).expect("source fixture is readable");
    let line = source
        .lines()
        .position(|line| line == "theorem knownTheorem : True := by")
        .expect("known theorem line is present");
    u32::try_from(line.checked_add(1).expect("fixture line index fits usize")).expect("fixture line fits u32")
}

fn assert_cancelled(err: LeanError) {
    assert_eq!(err.code(), LeanDiagnosticCode::Cancelled);
    match err {
        LeanError::Cancelled(_) => {}
        LeanError::Host(failure) => panic!("expected LeanError::Cancelled, got Host {failure:?}"),
        LeanError::LeanException(exc) => panic!("expected LeanError::Cancelled, got LeanException {exc:?}"),
    }
}

#[test]
fn declaration_source_range_returns_known_one_based_fixture_range() {
    let host = fixture_host();
    let caps = host
        .load_capabilities("lean_rs_fixture", "LeanRsFixture")
        .expect("load caps");
    let mut session = session_over_source_ranges(&caps);

    let range = session
        .declaration_source_range("LeanRsFixture.SourceRanges.knownTheorem", None)
        .expect("range query succeeds")
        .expect("known theorem has a declaration range");

    assert!(
        Path::new(&range.file).ends_with(Path::new("LeanRsFixture").join("SourceRanges.lean")),
        "range file should resolve to fixture source, got {}",
        range.file,
    );
    assert_eq!(range.start_line, expected_known_theorem_line());
    assert_eq!(range.start_column, 1, "columns are exposed as 1-based");
    assert!(range.end_line >= range.start_line);
    assert!(range.end_column >= 1, "end column is exposed as 1-based");
}

#[test]
fn declaration_source_range_returns_none_for_synthetic_and_missing_names() {
    let host = fixture_host();
    let caps = host
        .load_capabilities("lean_rs_fixture", "LeanRsFixture")
        .expect("load caps");
    let mut session = session_over_source_ranges(&caps);

    let synthetic = session
        .declaration_source_range("LeanRsFixture.SourceRanges.syntheticNoRange", None)
        .expect("synthetic range query succeeds");
    assert!(
        synthetic.is_none(),
        "run_cmd-added synthetic declaration has no source range"
    );

    let missing = session
        .declaration_source_range("LeanRsFixture.SourceRanges.missingNoRange", None)
        .expect("missing range query succeeds");
    assert!(missing.is_none(), "missing declaration has no source range");
}

#[test]
fn list_declarations_filtered_applies_each_flag_in_lean() {
    let host = fixture_host();
    let caps = host
        .load_capabilities("lean_rs_fixture", "LeanRsFixture")
        .expect("load caps");
    let mut session = session_over_source_ranges(&caps);

    let unfiltered = session.list_declarations(None).expect("unfiltered list").len();
    let default = session
        .list_declarations_filtered(&LeanDeclarationFilter::default(), None, None)
        .expect("default filtered list")
        .len();
    assert!(default <= unfiltered, "filtered list must not exceed unfiltered list");

    let without_private = session
        .list_declarations_filtered(
            &LeanDeclarationFilter {
                include_private: false,
                ..LeanDeclarationFilter::default()
            },
            None,
            None,
        )
        .expect("private-filtered list")
        .len();
    assert!(
        without_private < default,
        "turning off private names must exclude private fixture declarations",
    );

    let with_generated = session
        .list_declarations_filtered(
            &LeanDeclarationFilter {
                include_generated: true,
                ..LeanDeclarationFilter::default()
            },
            None,
            None,
        )
        .expect("generated-inclusive list")
        .len();
    assert!(
        with_generated > default,
        "turning on generated names must admit generated fixture declarations",
    );

    let with_internal = session
        .list_declarations_filtered(
            &LeanDeclarationFilter {
                include_internal: true,
                ..LeanDeclarationFilter::default()
            },
            None,
            None,
        )
        .expect("internal-inclusive list")
        .len();
    assert!(
        with_internal > default,
        "turning on internal names must admit internal fixture declarations",
    );
}

#[test]
fn list_declarations_filtered_records_one_dispatch() {
    let host = fixture_host();
    let caps = host
        .load_capabilities("lean_rs_fixture", "LeanRsFixture")
        .expect("load caps");
    let mut session = session_over_source_ranges(&caps);

    let before = session.stats();
    let names = session
        .list_declarations_filtered(&LeanDeclarationFilter::default(), None, None)
        .expect("filtered list succeeds");
    assert!(!names.is_empty(), "filtered fixture environment is non-empty");
    let after = session.stats();

    assert_eq!(after.ffi_calls - before.ffi_calls, 1);
    assert_eq!(after.batch_items - before.batch_items, 0);
}

#[test]
fn pre_cancelled_token_cancels_before_source_range_or_filter_dispatch() {
    let host = fixture_host();
    let caps = host
        .load_capabilities("lean_rs_fixture", "LeanRsFixture")
        .expect("load caps");
    let mut session = session_over_source_ranges(&caps);
    let token = LeanCancellationToken::new();
    token.cancel();

    let before = session.stats();
    let err = session
        .declaration_source_range("LeanRsFixture.SourceRanges.knownTheorem", Some(&token))
        .expect_err("pre-cancelled source-range query must fail");
    assert_cancelled(err);
    assert_eq!(
        session.stats(),
        before,
        "pre-cancelled source range records no FFI call"
    );

    let err = session
        .list_declarations_filtered(&LeanDeclarationFilter::default(), Some(&token), None)
        .expect_err("pre-cancelled filtered listing must fail");
    assert_cancelled(err);
    assert_eq!(
        session.stats(),
        before,
        "pre-cancelled filtered list records no FFI call"
    );
}
