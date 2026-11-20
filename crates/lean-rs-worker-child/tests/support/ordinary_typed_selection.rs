//! Checked launch inputs shared by the named SDK-owned typed tests.
use std::fmt::Write as _;
use std::fs;
use std::path::PathBuf;

/// Ordinary SDK-owned deployment. The app verifies its sealed images and imports
/// under real reader locks; this launcher only selects that exact worker and shim descriptor.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct OrdinaryTypedSelection {
    pub(super) worker: PathBuf,
    worker_sha256: String,
    pub(super) fixture: PathBuf,
    pub(super) json_manifest: PathBuf,
    pub(super) sysroot: PathBuf,
    pub(super) prepared: Option<lean_rs_worker_protocol::types::LeanWorkerPreparedHostArtifacts>,
}

#[allow(dead_code, reason = "streaming-only test also shares the parser")]
pub(super) fn ordinary_typed_selection() -> Option<&'static OrdinaryTypedSelection> {
    static SELECTED: std::sync::OnceLock<Option<OrdinaryTypedSelection>> = std::sync::OnceLock::new();
    SELECTED
        .get_or_init(|| option_env!("LEAN_RS_TEST_ORDINARY_TYPED_SESSION").map(|raw| parse_selection(raw, true)))
        .as_ref()
}

#[allow(dead_code, reason = "streaming tests only")]
pub(super) fn ordinary_stream_selection() -> Option<&'static OrdinaryTypedSelection> {
    static SELECTED: std::sync::OnceLock<Option<OrdinaryTypedSelection>> = std::sync::OnceLock::new();
    SELECTED
        .get_or_init(|| option_env!("LEAN_RS_TEST_ORDINARY_STREAM").map(|raw| parse_selection(raw, false)))
        .as_ref()
}

fn parse_selection(raw: &str, needs_prepared: bool) -> OrdinaryTypedSelection {
    assert!(raw.len() <= 64 * 1024, "typed selection exceeds launch limit");
    let selected: OrdinaryTypedSelection = serde_json::from_str(raw).expect("valid typed fixture selection");
    assert_eq!(selected.prepared.is_some(), needs_prepared);
    for path in [
        &selected.worker,
        &selected.fixture,
        &selected.sysroot,
        &selected.json_manifest,
    ] {
        assert_eq!(path.canonicalize().expect("canonical typed input"), *path);
    }
    checked_worker(&selected.worker, &selected.worker_sha256);
    selected
}

#[allow(dead_code, reason = "scalar protocol and supervisor tests only")]
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct OrdinaryScalarSelection {
    pub(super) worker: PathBuf,
    worker_sha256: String,
    pub(super) fixture: PathBuf,
    sysroot: PathBuf,
}

#[allow(dead_code, reason = "scalar protocol and supervisor tests only")]
pub(super) fn ordinary_scalar_selection() -> Option<&'static OrdinaryScalarSelection> {
    static SELECTED: std::sync::OnceLock<Option<OrdinaryScalarSelection>> = std::sync::OnceLock::new();
    SELECTED
        .get_or_init(|| {
            option_env!("LEAN_RS_TEST_ORDINARY_SCALAR").map(|raw| {
                assert!(raw.len() <= 64 * 1024, "scalar selection exceeds launch limit");
                let selected: OrdinaryScalarSelection = serde_json::from_str(raw).expect("valid scalar selection");
                for path in [&selected.worker, &selected.fixture, &selected.sysroot] {
                    assert_eq!(path.canonicalize().expect("canonical scalar input"), *path);
                }
                assert_eq!(
                    std::env::var_os("LEAN_SYSROOT").as_deref(),
                    Some(selected.sysroot.as_os_str())
                );
                assert!(std::env::var_os("LEAN_PATH").is_none());
                checked_worker(&selected.worker, &selected.worker_sha256);
                selected
            })
        })
        .as_ref()
}

fn checked_worker(worker: &std::path::Path, expected: &str) {
    use sha2::{Digest, Sha256};
    use std::io::Read as _;
    let mut file = fs::File::open(worker).expect("open selected typed worker");
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
    assert_eq!(actual, expected, "selected fixture worker changed");
}
