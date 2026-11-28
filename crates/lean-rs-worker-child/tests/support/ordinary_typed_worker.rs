//! Worker configuration for the ordinary SDK host session tests.
use lean_rs_worker_parent::LeanWorkerConfig;

use super::ordinary_typed_selection::{OrdinaryTypedSelection, ordinary_typed_selection};

pub(super) fn worker_config() -> Option<LeanWorkerConfig> {
    ordinary_typed_selection().map(sealed_worker_config)
}

fn sealed_worker_config(selected: &OrdinaryTypedSelection) -> LeanWorkerConfig {
    assert!(std::env::var_os("LEAN_PATH").is_none());
    LeanWorkerConfig::new(&selected.worker)
        .checked_native_loader_environment()
        .env("LEAN_SYSROOT", selected.sysroot.as_os_str())
        .env(
            lean_rs_worker_protocol::PREPARED_HOST_ARTIFACTS_ENV,
            serde_json::to_string(&selected.prepared).expect("prepared shim descriptor"),
        )
}
