//! Checked launch inputs for the typed session tests the qualifier selects.
use std::path::PathBuf;

use super::checked_worker::checked_worker;

/// Ordinary SDK-owned deployment. The app verifies its sealed images before it
/// imports them; this launcher only selects that exact worker and shim descriptor.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct OrdinaryTypedSelection {
    pub(super) worker: PathBuf,
    worker_sha256: String,
    pub(super) fixture: PathBuf,
    pub(super) sysroot: PathBuf,
    pub(super) prepared: lean_rs_worker_protocol::types::LeanWorkerPreparedHostArtifacts,
}

/// The selection in `LEAN_RS_TEST_ORDINARY_TYPED_SESSION`, parsed and checked
/// once, or `None` when the qualifier did not select a typed deployment.
pub(super) fn ordinary_typed_selection() -> Option<&'static OrdinaryTypedSelection> {
    static SELECTED: std::sync::OnceLock<Option<OrdinaryTypedSelection>> = std::sync::OnceLock::new();
    SELECTED
        .get_or_init(|| option_env!("LEAN_RS_TEST_ORDINARY_TYPED_SESSION").map(parse_selection))
        .as_ref()
}

fn parse_selection(raw: &str) -> OrdinaryTypedSelection {
    assert!(raw.len() <= 64 * 1024, "typed selection exceeds launch limit");
    let selected: OrdinaryTypedSelection = serde_json::from_str(raw).expect("valid typed fixture selection");
    for path in [&selected.worker, &selected.fixture, &selected.sysroot] {
        assert_eq!(path.canonicalize().expect("canonical typed input"), *path);
    }
    checked_worker(&selected.worker, &selected.worker_sha256);
    selected
}
