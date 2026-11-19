//! Launch inputs embedded by the named fixture qualification.
use lean_rs_worker_protocol::types::LeanWorkerPreparedHostArtifacts;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Qualification {
    pub(super) worker: PathBuf,
    worker_sha256: String,
    pub(super) helper: PathBuf,
    helper_sha256: String,
    pub(super) owners: PathBuf,
    pub(super) sysroot: PathBuf,
    #[serde(alias = "manifest")]
    pub(super) fixture: PathBuf,
    pub(super) prepared: LeanWorkerPreparedHostArtifacts,
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

pub(super) fn checked(selection: &str) -> Qualification {
    let q: Qualification = serde_json::from_str(selection).expect("invalid compiled fixture qualification");
    assert!(
        !std::env::vars_os().any(|(key, _)| key.as_encoded_bytes().starts_with(b"DYLD_"))
            && std::env::var_os("LEAN_PATH").is_none(),
        "fixture qualification requires the selected loader and import environment"
    );
    check_executable(&q.worker, &q.worker_sha256);
    check_executable(&q.helper, &q.helper_sha256);
    q
}
