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
    let state = q.owners.parent().expect("qualified owners have a state directory");
    let qualification = state.parent().expect("qualified state has a qualification directory");
    let target = qualification
        .parent()
        .expect("qualified state is below the Cargo target");
    assert_eq!(q.owners.file_name().and_then(|name| name.to_str()), Some("owners"));
    assert_eq!(state.file_name().and_then(|name| name.to_str()), Some("ordinary"));
    assert_eq!(
        qualification.file_name().and_then(|name| name.to_str()),
        Some("metadata-qualification")
    );
    let socket_root = target.join("ws");
    let child = LeanWorkerChild::for_toolchain(q.worker, q.sysroot)
        .prepared_host_artifacts(q.prepared)
        .shared_writer_executable(q.helper.clone())
        .native_guardian(q.helper)
        .lifetime_directory(q.owners)
        .lifetime_socket_directory(socket_root)
        .shared_artifact_reads(true);
    LeanWorkerCapabilityBuilder::from_built_capability(
        &LeanBuiltCapability::manifest_path(q.fixture),
        ["LeanRsInteropConsumer.Callback"],
    )
    .expect("select the prepared metadata fixture")
    .worker_child(child)
}
