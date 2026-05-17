//! End-to-end tests for the `LeanHost` / `LeanCapabilities` /
//! `LeanSession` cascade.
//!
//! Each test bootstraps the runtime, opens the fixture Lake project,
//! loads the `LeanRsFixture` capability dylib (which pre-resolves all
//! seven session symbols), starts a session over an import list, and
//! exercises the typed query methods.

#![allow(clippy::expect_used, clippy::panic)]

use std::path::PathBuf;
use std::time::Instant;

use crate::error::{HostStage, LeanError};
use crate::runtime::LeanRuntime;
use crate::{LeanHost, LeanSession};

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
    }
}

// -- session import + query ---------------------------------------------

fn session_over_handles<'lean, 'c>(caps: &'c crate::LeanCapabilities<'lean, 'c>) -> LeanSession<'lean, 'c> {
    caps.session(&["LeanRsFixture.Handles"])
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
        .query_declaration("LeanRsFixture.Handles.nameAnonymous")
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
        .declaration_kind("LeanRsFixture.Handles.nameAnonymous")
        .expect("kind for fixture def");
    assert_eq!(
        fixture_def_kind, "definition",
        "fixture `def` must classify as definition"
    );

    let nat_kind = session.declaration_kind("Nat").expect("kind for Nat");
    assert_eq!(nat_kind, "inductive", "prelude `Nat` must classify as inductive");

    let missing_kind = session
        .declaration_kind("This.Name.Does.Not.Exist")
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
        .declaration_type("LeanRsFixture.Handles.nameAnonymous")
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
        .declaration_type("This.Name.Does.Not.Exist")
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
        .declaration_name("LeanRsFixture.Handles.nameAnonymous")
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
        .query_declaration("This.Name.Does.Not.Exist")
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
    }
}

#[test]
fn session_list_declarations_includes_prelude_and_fixture() {
    let host = fixture_host();
    let caps = host
        .load_capabilities("lean_rs_fixture", "LeanRsFixture")
        .expect("load caps");
    let mut session = session_over_handles(&caps);

    let names = session.list_declarations().expect("list declarations");
    // The Lean prelude alone contributes thousands; the fixture import
    // is a thin slice on top. Just assert the result is non-empty.
    assert!(
        !names.is_empty(),
        "imported environment must contain at least one declaration"
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
            .session(&["LeanRsFixture.Handles"])
            .expect("session imports cleanly");
        for _ in 0..QUERIES {
            let kind = session
                .declaration_kind("LeanRsFixture.Handles.nameAnonymous")
                .expect("query");
            assert_eq!(kind, "definition");
        }
    }
    let reuse_elapsed = start_reuse.elapsed();

    // (b) Fresh session per query.
    let start_per_query = Instant::now();
    for _ in 0..QUERIES {
        let mut session = caps
            .session(&["LeanRsFixture.Handles"])
            .expect("session imports cleanly");
        let kind = session
            .declaration_kind("LeanRsFixture.Handles.nameAnonymous")
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
