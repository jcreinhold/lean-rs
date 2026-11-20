//! Build a worker from the exact ordinary interop selection, when present.
use lean_rs_worker_parent::{LeanWorkerCapabilityBuilder, LeanWorkerChild};

#[path = "ordinary_typed_selection.rs"]
mod ordinary_typed_selection;

pub(super) fn selected() -> Option<LeanWorkerCapabilityBuilder> {
    ordinary_typed_selection::ordinary_stream_selection().map(sealed_builder)
}

fn sealed_builder(selected: &ordinary_typed_selection::OrdinaryTypedSelection) -> LeanWorkerCapabilityBuilder {
    let child = LeanWorkerChild::for_toolchain(&selected.worker, &selected.sysroot).checked_native_loader_environment();
    LeanWorkerCapabilityBuilder::from_built_capability(
        &lean_rs::LeanBuiltCapability::manifest_path(&selected.json_manifest),
        ["LeanRsInteropConsumer.Callback"],
    )
    .expect("select sealed interop capability")
    .worker_child(child)
}
