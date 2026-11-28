//! Checked launch inputs for the scalar tests the qualifier selects.
use std::path::PathBuf;

use super::checked_worker::checked_worker;

/// Sealed scalar deployment: a worker and fixture the tests launch with the
/// selected toolchain and no shim descriptor.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct OrdinaryScalarSelection {
    pub(super) worker: PathBuf,
    worker_sha256: String,
    pub(super) fixture: PathBuf,
    sysroot: PathBuf,
}

/// The selection in `LEAN_RS_TEST_ORDINARY_SCALAR`, parsed and checked once,
/// or `None` when the qualifier did not select a scalar deployment.
pub(super) fn ordinary_scalar_selection() -> Option<&'static OrdinaryScalarSelection> {
    static SELECTED: std::sync::OnceLock<Option<OrdinaryScalarSelection>> = std::sync::OnceLock::new();
    SELECTED
        .get_or_init(|| option_env!("LEAN_RS_TEST_ORDINARY_SCALAR").map(parse_selection))
        .as_ref()
}

fn parse_selection(raw: &str) -> OrdinaryScalarSelection {
    assert!(raw.len() <= 64 * 1024, "direct selection exceeds launch limit");
    let selected: OrdinaryScalarSelection = serde_json::from_str(raw).expect("valid direct fixture selection");
    for path in [&selected.worker, &selected.fixture, &selected.sysroot] {
        assert_eq!(path.canonicalize().expect("canonical direct input"), *path);
    }
    assert_eq!(
        std::env::var_os("LEAN_SYSROOT").as_deref(),
        Some(selected.sysroot.as_os_str())
    );
    assert!(std::env::var_os("LEAN_PATH").is_none());
    checked_worker(&selected.worker, &selected.worker_sha256);
    selected
}
