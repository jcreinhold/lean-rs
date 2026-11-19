//! Checked launch inputs shared by the named SDK-owned typed tests.
use lean_rs_worker_parent::LeanWorkerConfig;
use std::fmt::Write as _;
use std::fs;
use std::path::PathBuf;

/// Ordinary SDK-owned deployment. The app verifies its sealed images and imports
/// under real reader locks; this launcher only selects that exact worker and shim descriptor.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct OrdinaryTypedSelection {
    worker: PathBuf,
    worker_sha256: String,
    pub(super) fixture: PathBuf,
    sysroot: PathBuf,
    prepared: lean_rs_worker_protocol::types::LeanWorkerPreparedHostArtifacts,
}

pub(super) fn ordinary_typed_selection() -> Option<&'static OrdinaryTypedSelection> {
    static SELECTED: std::sync::OnceLock<Option<OrdinaryTypedSelection>> = std::sync::OnceLock::new();
    SELECTED
        .get_or_init(|| {
            let raw = option_env!("LEAN_RS_TEST_ORDINARY_TYPED_SESSION")?;
            assert!(raw.len() <= 64 * 1024, "typed selection exceeds launch limit");
            let selected: OrdinaryTypedSelection = serde_json::from_str(raw).expect("valid typed fixture selection");
            for path in [&selected.worker, &selected.fixture, &selected.sysroot] {
                assert_eq!(path.canonicalize().expect("canonical typed input"), *path);
            }
            use sha2::{Digest, Sha256};
            use std::io::Read as _;
            let mut file = fs::File::open(&selected.worker).expect("open selected typed worker");
            let metadata = file.metadata().expect("selected worker metadata");
            assert!(metadata.is_file() && metadata.len() <= 256 * 1024 * 1024);
            let mut remaining = metadata.len();
            let mut digest = Sha256::new();
            let mut buffer = [0_u8; 16 * 1024];
            loop {
                let count = file.read(&mut buffer).expect("read selected typed worker");
                if count == 0 {
                    break;
                }
                remaining = remaining.checked_sub(count as u64).expect("selected worker grew");
                digest.update(buffer.get(..count).expect("read count fits buffer"));
            }
            assert_eq!(remaining, 0, "selected worker shrank");
            let mut actual = String::new();
            for byte in digest.finalize() {
                write!(&mut actual, "{byte:02x}").expect("digest text");
            }
            assert_eq!(actual, selected.worker_sha256, "selected typed worker changed");
            Some(selected)
        })
        .as_ref()
}

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
