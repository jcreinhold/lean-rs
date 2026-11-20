//! Worker configuration for the ordinary SDK host session tests.
use lean_rs_worker_parent::LeanWorkerConfig;

use super::ordinary_typed_selection::ordinary_typed_selection;

pub(super) fn worker_config() -> Option<LeanWorkerConfig> {
    if let Some(selected) = ordinary_typed_selection() {
        assert!(std::env::var_os("LEAN_PATH").is_none());
        return Some(
            LeanWorkerConfig::new(&selected.worker)
                .checked_native_loader_environment()
                .env("LEAN_SYSROOT", selected.sysroot.as_os_str())
                .env(
                    "LEAN_RS_PREPARED_HOST_ARTIFACTS",
                    serde_json::to_string(&selected.prepared).expect("prepared shim descriptor"),
                ),
        );
    }
    None
}
