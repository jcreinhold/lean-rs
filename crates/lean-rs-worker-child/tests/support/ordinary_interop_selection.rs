//! Checked launch inputs for the interop tests the qualifier selects.
use std::path::PathBuf;

use super::checked_worker::checked_worker;

/// Sealed interop deployment: the fixture worker, the interop project in the
/// shim package, and the capability manifest the qualifier wrote for it.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct OrdinaryInteropSelection {
    pub(super) worker: PathBuf,
    worker_sha256: String,
    pub(super) fixture: PathBuf,
    pub(super) sysroot: PathBuf,
    pub(super) capability_manifest: PathBuf,
}

/// The selection in `LEAN_RS_TEST_ORDINARY_INTEROP`, parsed and checked once,
/// or `None` when the qualifier did not select an interop deployment.
pub(super) fn ordinary_interop_selection() -> Option<&'static OrdinaryInteropSelection> {
    static SELECTED: std::sync::OnceLock<Option<OrdinaryInteropSelection>> = std::sync::OnceLock::new();
    SELECTED
        .get_or_init(|| option_env!("LEAN_RS_TEST_ORDINARY_INTEROP").map(parse_selection))
        .as_ref()
}

fn parse_selection(raw: &str) -> OrdinaryInteropSelection {
    assert!(raw.len() <= 64 * 1024, "interop selection exceeds launch limit");
    let selected: OrdinaryInteropSelection = serde_json::from_str(raw).expect("valid interop fixture selection");
    for path in [
        &selected.worker,
        &selected.fixture,
        &selected.sysroot,
        &selected.capability_manifest,
    ] {
        assert_eq!(path.canonicalize().expect("canonical interop input"), *path);
    }
    checked_worker(&selected.worker, &selected.worker_sha256);
    selected
}
