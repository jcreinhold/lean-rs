//! Runtime-free shim preparation and the prepared descriptor a parent passes at launch.
use std::ffi::OsStr;
use std::fs::File;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use lean_rs_host::host::{
    LeanPreparedHostArtifacts, prepare_host_artifacts, prepare_host_artifacts_in, ready_host_artifacts,
};
use lean_rs_worker_protocol::types::LeanWorkerPreparedHostArtifacts;

const MAX_DESCRIPTOR_BYTES: usize = 64 * 1024;
/// The only descriptor layout this child writes or reads.
const DESCRIPTOR_PROTOCOL: u32 = 1;
pub(crate) const PREPARED_ENV: &str = "LEAN_RS_PREPARED_HOST_ARTIFACTS";

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

/// Serve a preparation flag, if the first argument is one, and return its exit code.
///
/// `--lean-rs-ready-host-artifacts` prints the descriptor of shims that Lake
/// reports up to date, or exits 75 when they are not; it builds nothing.
/// `--lean-rs-prepare-host-artifacts [DEST]` builds the shims, in `DEST`
/// when given. This runs before the Lean runtime or the parent lifeline
/// starts.
pub(crate) fn preparation_mode() -> Option<ExitCode> {
    let arg = std::env::args_os().nth(1)?;
    let probe = if arg == "--lean-rs-ready-host-artifacts" {
        true
    } else if arg == "--lean-rs-prepare-host-artifacts" {
        false
    } else {
        return None;
    };
    let result = (|| -> Result<Option<LeanPreparedHostArtifacts>, Box<dyn std::error::Error>> {
        let args = std::env::args_os().collect::<Vec<_>>();
        let valid_args = if probe {
            args.len() == 2
        } else {
            args.len() == 2 || args.len() == 3
        };
        if !valid_args {
            return Err(invalid("artifact preparation got the wrong number of arguments").into());
        }
        if probe {
            Ok(ready_host_artifacts()?)
        } else if let Some(destination) = args.get(2) {
            Ok(Some(prepare_host_artifacts_in(Path::new(destination))?))
        } else {
            Ok(Some(prepare_host_artifacts()?))
        }
    })();
    Some(match result {
        Ok(Some(prepared)) => match write_descriptor(&prepared) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("lean-rs artifact preparation: {error}");
                ExitCode::FAILURE
            }
        },
        Ok(None) => ExitCode::from(75),
        Err(error) => {
            eprintln!("lean-rs artifact preparation: {error}");
            ExitCode::FAILURE
        }
    })
}

fn write_descriptor(prepared: &LeanPreparedHostArtifacts) -> io::Result<()> {
    let bytes = descriptor_bytes(prepared)?;
    let mut stdout = std::io::stdout().lock();
    stdout.write_all(&bytes)?;
    stdout.write_all(b"\n")
}

fn descriptor_bytes(prepared: &LeanPreparedHostArtifacts) -> io::Result<Vec<u8>> {
    let path_string = |path: &Path| {
        path.to_str()
            .map(str::to_owned)
            .ok_or_else(|| invalid("artifact descriptor paths must be UTF-8"))
    };
    let dto = LeanWorkerPreparedHostArtifacts {
        protocol: DESCRIPTOR_PROTOCOL,
        shim_manifest_path: path_string(prepared.manifest_path())?,
        shim_manifest_json: prepared.manifest_json().to_owned(),
        shim_search_paths: prepared
            .shim_search_paths()
            .iter()
            .map(|path| path_string(path))
            .collect::<io::Result<_>>()?,
    };
    let bytes = serde_json::to_vec(&dto).map_err(io::Error::other)?;
    if bytes.len() > MAX_DESCRIPTOR_BYTES {
        return Err(invalid("prepared descriptor exceeds 64 KiB"));
    }
    Ok(bytes)
}

/// Parse the descriptor the parent passed in `LEAN_RS_PREPARED_HOST_ARTIFACTS`.
///
/// The named manifest must still hold the descriptor's bytes.
pub(crate) fn from_launch(value: Option<&OsStr>) -> io::Result<Option<LeanPreparedHostArtifacts>> {
    let Some(value) = value else {
        return Ok(None);
    };
    let value = value
        .to_str()
        .ok_or_else(|| invalid("prepared descriptor must be UTF-8"))?;
    if value.len() > MAX_DESCRIPTOR_BYTES {
        return Err(invalid("prepared descriptor exceeds 64 KiB"));
    }
    let dto: LeanWorkerPreparedHostArtifacts = serde_json::from_str(value).map_err(io::Error::other)?;
    if dto.protocol != DESCRIPTOR_PROTOCOL {
        return Err(invalid("unsupported prepared artifact protocol"));
    }
    let prepared = LeanPreparedHostArtifacts::from_snapshot(
        PathBuf::from(dto.shim_manifest_path),
        dto.shim_manifest_json,
        dto.shim_search_paths.into_iter().map(PathBuf::from).collect(),
    )
    .map_err(io::Error::other)?;
    let path = prepared.manifest_path();
    if path.canonicalize()? != path || read_bounded(path)? != prepared.manifest_json().as_bytes() {
        return Err(invalid("prepared host manifest differs from its launch snapshot"));
    }
    Ok(Some(prepared))
}

/// Read a regular file of at most 64 KiB without following a final symlink
/// or waiting on a FIFO.
pub(crate) fn read_bounded(path: &Path) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    use std::os::unix::fs::OpenOptionsExt as _;
    let file = File::options()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW)
        .open(path)?;
    if !file.metadata()?.is_file() {
        return Err(invalid("capability metadata requires a regular file"));
    }
    file.take((MAX_DESCRIPTOR_BYTES + 1) as u64).read_to_end(&mut bytes)?;
    if bytes.len() > MAX_DESCRIPTOR_BYTES {
        return Err(invalid("capability metadata exceeds 64 KiB"));
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::panic, clippy::indexing_slicing)]
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};
    static NONCE: AtomicU64 = AtomicU64::new(0);
    #[test]
    fn refused_manifest_reads_never_reach_the_native_loader() {
        use crate::WorkerManifestRead;
        let fixture = Fixture::new();
        let path = fixture.host.join("manifest.fifo");
        let status = std::process::Command::new("mkfifo")
            .arg(&path)
            .status()
            .expect("create FIFO");
        assert!(status.success());
        let custody = WorkerManifestRead::new(fixture.host.clone(), path.clone(), ());
        let mut calls = 0;
        let mut initialize = |_: &Path, _: &[u8]| {
            calls += 1;
            Ok(())
        };
        let outside = fixture.project.join("missing.json");
        assert!(custody.read_for(&outside, &mut initialize).is_err());
        let error = custody
            .read_for(&path, &mut initialize)
            .expect_err("FIFO must be rejected without a writer");
        assert!(error.to_string().contains("regular file"));
        assert_eq!(calls, 0);
        let regular = fixture.host.join("manifest.json");
        fs::write(&regular, b"{}").expect("write regular file");
        let other_root = WorkerManifestRead::new(fixture.project.clone(), regular.clone(), ());
        assert!(
            other_root
                .read_for::<()>(&regular, |_, _| panic!("outside approved root"))
                .is_err()
        );
        WorkerManifestRead::new(fixture.host.clone(), regular.clone(), ())
            .read_for(&regular, |_, bytes| {
                assert_eq!(bytes, b"{}");
                calls += 1;
                Ok(())
            })
            .expect("regular file admitted");
        assert_eq!(calls, 1);
    }

    struct Fixture {
        directory: PathBuf,
        host: PathBuf,
        interop: PathBuf,
        project: PathBuf,
    }
    impl Fixture {
        fn new() -> Self {
            let directory = std::env::temp_dir().join(format!(
                "lean-preload-{}-{}",
                std::process::id(),
                NONCE.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&directory).expect("temp directory");
            let directory = directory.canonicalize().expect("canonical temp");
            let root = |name| {
                let root = directory.join(name);
                fs::create_dir_all(root.join("out/lib/lean")).expect("output directory");
                fs::write(root.join("out/lib/libcap.dylib"), b"not loaded by metadata tests").expect("artifact");
                root
            };
            Self {
                host: root("host"),
                interop: root("interop"),
                project: root("project"),
                directory,
            }
        }
        fn manifest(root: &Path, package: &str, module: &str, deps: &[PathBuf]) -> PathBuf {
            let path = root.join("out/capability.json");
            fs::write(&path,serde_json::to_vec(&serde_json::json!({
                "schema_version":lean_toolchain::CAPABILITY_MANIFEST_SCHEMA_VERSION,
                "primary_dylib":root.join("out/lib/libcap.dylib"),"package":package,"module":module,
                "dependencies":deps.iter().map(|root|serde_json::json!({"dylib_path":root.join("out/lib/libcap.dylib")})).collect::<Vec<_>>(),"exports":[]
            })).expect("manifest JSON")).expect("manifest");
            path
        }
        fn prepared(&self) -> LeanPreparedHostArtifacts {
            let manifest = Self::manifest(
                &self.host,
                "lean_rs_host_shims",
                "LeanRsHostShims",
                std::slice::from_ref(&self.interop),
            );
            LeanPreparedHostArtifacts::from_snapshot(
                manifest.clone(),
                fs::read_to_string(manifest).expect("snapshot"),
                vec![self.interop.join("out/lib/lean"), self.host.join("out/lib/lean")],
            )
            .expect("storage descriptor")
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _removed = fs::remove_dir_all(&self.directory);
        }
    }

    #[test]
    fn launch_selects_the_exact_prepared_snapshot_or_refuses() {
        use std::os::unix::ffi::OsStrExt as _;

        let fixture = Fixture::new();
        let expected = fixture.prepared();
        let bytes = descriptor_bytes(&expected).expect("encode deployment outside bundled roots");
        let explicit = Some(OsStr::from_bytes(&bytes));
        let actual = from_launch(explicit).expect("launch").expect("explicit snapshot");
        assert_eq!(actual.manifest_path(), expected.manifest_path());
        assert_eq!(actual.manifest_json(), expected.manifest_json());
        assert_eq!(actual.shim_search_paths(), expected.shim_search_paths());
        assert!(from_launch(None).expect("default").is_none());
        assert!(from_launch(Some(OsStr::new(""))).is_err());
        assert!(from_launch(Some(OsStr::from_bytes(&[0xff]))).is_err());

        fs::write(expected.manifest_path(), b"{}").expect("replace named manifest");
        assert!(
            from_launch(explicit)
                .expect_err("stale snapshot")
                .to_string()
                .contains("differs")
        );
        fs::remove_file(expected.manifest_path()).expect("remove named manifest");
        assert_eq!(
            from_launch(explicit).expect_err("missing manifest").kind(),
            io::ErrorKind::NotFound
        );
    }

    #[test]
    fn launch_refuses_a_descriptor_of_another_protocol() {
        use std::os::unix::ffi::OsStrExt as _;

        let fixture = Fixture::new();
        let bytes = descriptor_bytes(&fixture.prepared()).expect("encode descriptor");
        let mut dto: LeanWorkerPreparedHostArtifacts = serde_json::from_slice(&bytes).expect("decode descriptor");
        assert_eq!(dto.protocol, DESCRIPTOR_PROTOCOL);
        dto.protocol = 2;
        let other = serde_json::to_vec(&dto).expect("encode other protocol");
        let error = from_launch(Some(OsStr::from_bytes(&other))).expect_err("other protocol");
        assert!(error.to_string().contains("unsupported prepared artifact protocol"));
    }
}
