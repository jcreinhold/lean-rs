//! Build a worker from the exact ordinary interop selection, when present.
use lean_rs_worker_parent::{LeanWorkerCapabilityBuilder, LeanWorkerChild};

use super::ordinary_typed_selection;

pub(super) fn selected() -> Option<LeanWorkerCapabilityBuilder> {
    selected_with_imports(["LeanRsInteropConsumer.Callback"])
}

pub(super) fn selected_with_imports(
    imports: impl IntoIterator<Item = &'static str>,
) -> Option<LeanWorkerCapabilityBuilder> {
    ordinary_typed_selection::ordinary_stream_selection().map(|selected| {
        let child =
            LeanWorkerChild::for_toolchain(&selected.worker, &selected.sysroot).checked_native_loader_environment();
        LeanWorkerCapabilityBuilder::from_built_capability(
            &lean_rs::LeanBuiltCapability::manifest_path(&selected.json_manifest),
            imports,
        )
        .expect("select sealed interop capability")
        .worker_child(child)
    })
}
