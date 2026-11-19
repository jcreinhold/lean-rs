//! The named metadata qualification embeds its reviewed launch inputs in the test binary.
use lean_rs::LeanBuiltCapability;
use lean_rs_worker_parent::{LeanWorkerCapabilityBuilder, LeanWorkerChild};
use lean_rs_worker_protocol::types::LeanWorkerPreparedHostArtifacts;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Qualification {
    worker: PathBuf,
    worker_sha256: String,
    helper: PathBuf,
    helper_sha256: String,
    owners: PathBuf,
    sysroot: PathBuf,
    manifest: PathBuf,
    prepared: LeanWorkerPreparedHostArtifacts,
}

fn check_executable(path: &Path, approved: &str) {
    assert_eq!(path.canonicalize().expect("canonical qualification executable"), path);
    let bytes = fs::read(path).expect("read qualification executable before launch");
    let mut actual = String::with_capacity(64);
    for byte in Sha256::digest(bytes) {
        write!(actual, "{byte:02x}").expect("write hexadecimal digest to String");
    }
    assert_eq!(actual, approved, "unapproved executable {path:?}");
}

pub(super) fn builder() -> LeanWorkerCapabilityBuilder {
    let Some(selection) = option_env!("LEAN_RS_TEST_QUALIFIED_METADATA") else {
        return super::builder();
    };
    let q: Qualification = serde_json::from_str(selection).expect("invalid compiled metadata qualification");
    assert!(
        !std::env::vars_os().any(|(key, _)| key.as_encoded_bytes().starts_with(b"DYLD_"))
            && std::env::var_os("LEAN_PATH").is_none(),
        "metadata qualification requires the selected loader and import environment"
    );
    check_executable(&q.worker, &q.worker_sha256);
    check_executable(&q.helper, &q.helper_sha256);
    let child = LeanWorkerChild::for_toolchain(q.worker, q.sysroot)
        .prepared_host_artifacts(q.prepared)
        .shared_writer_executable(q.helper.clone())
        .native_guardian(q.helper)
        .lifetime_directory(q.owners)
        .shared_artifact_reads(true);
    LeanWorkerCapabilityBuilder::from_built_capability(
        &LeanBuiltCapability::manifest_path(q.manifest),
        ["LeanRsInteropConsumer.Callback"],
    )
    .expect("select the prepared metadata fixture")
    .worker_child(child)
}
