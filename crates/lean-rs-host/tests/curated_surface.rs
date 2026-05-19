#![allow(clippy::expect_used, clippy::panic)]
//! End-to-end check that the curated `lean_rs::*` surface alone is
//! sufficient to drive a Lean capability from runtime init through
//! kernel-checked evidence and back to a `ProofSummary`.
//!
//! This test deliberately uses only `use lean_rs::{...};` imports — no
//! `lean_rs::host::*`, `lean_rs::module::*`, or `lean_rs::error::*`
//! paths. If it fails to compile because a name is missing, the curated
//! surface in `crates/lean-rs/src/lib.rs` and the classification table
//! in `docs/architecture/04-host-stack.md` are out of sync and one of
//! them must be brought into agreement before widening the imports here.

use std::path::PathBuf;

use lean_rs::LeanRuntime;
use lean_rs_host::{EvidenceStatus, LeanElabOptions, LeanHost, LeanKernelOutcome};

fn fixture_lake_root() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace = manifest_dir
        .parent()
        .and_then(std::path::Path::parent)
        .expect("crates/<name>/ lives two directories beneath the workspace root");
    workspace.join("fixtures").join("lean")
}

#[test]
fn curated_surface_drives_full_happy_path() {
    let runtime = LeanRuntime::init().expect("Lean runtime initialisation must succeed");

    let host = LeanHost::from_lake_project(runtime, fixture_lake_root()).expect("fixture Lake project opens cleanly");
    let caps = host
        .load_capabilities("lean_rs_fixture", "LeanRsFixture")
        .expect("fixture capability loads cleanly");
    let mut session = caps
        .session(&["LeanRsHostShims.Elaboration"], None)
        .expect("LeanRsHostShims.Elaboration module imports cleanly");

    let _decl = session
        .query_declaration("Nat.add", None)
        .expect("Nat.add is in the imported environment");

    let opts = LeanElabOptions::new();

    let elab = session
        .elaborate("(1 + 1 : Nat)", None, &opts, None)
        .expect("host stack reports no exception while elaborating");
    elab.expect("elaboration succeeds for a well-typed Nat term");

    let outcome = session
        .kernel_check("theorem lean_rs_curated : 1 + 1 = 2 := rfl", &opts, None)
        .expect("host stack reports no exception while kernel-checking");

    let LeanKernelOutcome::Checked(evidence) = outcome else {
        panic!("expected Checked outcome for a closed reflexivity proof, got {outcome:?}");
    };

    let status = session
        .check_evidence(&evidence, None)
        .expect("re-validation dispatches cleanly");
    assert_eq!(status, EvidenceStatus::Checked, "evidence must re-validate as Checked");

    let summary = session
        .summarize_evidence(&evidence, None)
        .expect("summary projection dispatches cleanly");
    assert_eq!(
        summary.declaration_name(),
        "lean_rs_curated",
        "summary names the captured declaration",
    );
    assert_eq!(summary.kind(), "theorem", "kind tag tracks the declaration form");
    assert!(
        !summary.type_signature().is_empty(),
        "type-signature projection must render the proposition",
    );
}
