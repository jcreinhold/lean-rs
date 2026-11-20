//! Manifest-backed launch for the ordinary SDK JSON fixture.
use lean_rs::LeanBuiltCapability;
use lean_rs_worker_parent::{LeanWorkerCapabilityBuilder, LeanWorkerChild};

#[path = "ordinary_typed_selection.rs"]
mod ordinary_typed_selection;

pub(super) fn json_builder() -> Option<LeanWorkerCapabilityBuilder> {
    ordinary_typed_selection::ordinary_typed_selection().map(sealed_json_builder)
}

fn sealed_json_builder(selected: &ordinary_typed_selection::OrdinaryTypedSelection) -> LeanWorkerCapabilityBuilder {
    assert!(std::env::var_os("LEAN_PATH").is_none());
    let child = LeanWorkerChild::for_toolchain(&selected.worker, &selected.sysroot)
        .checked_native_loader_environment()
        .prepared_host_artifacts(selected.prepared.clone().expect("prepared shim descriptor"));
    LeanWorkerCapabilityBuilder::from_built_capability(
        &LeanBuiltCapability::manifest_path(&selected.json_manifest),
        ["LeanRsFixture.Handles", "LeanRsFixture.SourceRanges"],
    )
    .expect("select the sealed ordinary JSON capability")
    .worker_child(child)
}
