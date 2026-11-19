//! The named metadata qualification embeds its reviewed launch inputs in the test binary.
use lean_rs::LeanBuiltCapability;
use lean_rs_worker_parent::{LeanWorkerCapabilityBuilder, LeanWorkerChild};

#[path = "qualification_launch.rs"]
mod qualification_launch;

pub(super) fn builder() -> LeanWorkerCapabilityBuilder {
    let Some(selection) = option_env!("LEAN_RS_TEST_QUALIFIED_METADATA") else {
        return super::builder();
    };
    let q = qualification_launch::checked(selection);
    let child = LeanWorkerChild::for_toolchain(q.worker, q.sysroot)
        .prepared_host_artifacts(q.prepared)
        .shared_writer_executable(q.helper.clone())
        .native_guardian(q.helper)
        .lifetime_directory(q.owners)
        .shared_artifact_reads(true);
    LeanWorkerCapabilityBuilder::from_built_capability(
        &LeanBuiltCapability::manifest_path(q.fixture),
        ["LeanRsInteropConsumer.Callback"],
    )
    .expect("select the prepared metadata fixture")
    .worker_child(child)
}
