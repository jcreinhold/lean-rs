//! Runtime-free preparation and complete, captured pre-load admission metadata.
use std::collections::BTreeSet;
use std::fs::File;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use lean_rs_host::host::{LeanPreparedHostArtifacts, prepare_host_artifacts, ready_host_artifacts};
use lean_rs_worker_protocol::protocol::HostSessionMode;
use lean_rs_worker_protocol::types::LeanWorkerPreparedHostArtifacts;
use lean_toolchain::CapabilityManifest;
use lean_toolchain::shared_writer::{SHARED_WRITER_EXECUTABLE_ENV, project_artifact_root_union};

const MAX_DESCRIPTOR_BYTES: usize = 64 * 1024;
const PREPARED_ENV: &str = "LEAN_RS_PREPARED_HOST_ARTIFACTS";

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

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
        if std::env::args_os().count() != 2 || std::env::var_os(SHARED_WRITER_EXECUTABLE_ENV).is_none() {
            return Err(
                invalid("artifact preparation requires its sole mode flag and an explicit writer helper").into(),
            );
        }
        // This branch precedes all runtime, parent-watchdog and native ownership
        // initialization. Any actual Lake work is owned by the configured H.
        if probe {
            Ok(ready_host_artifacts()?)
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
    let path_string = |path: &Path| {
        path.to_str()
            .map(str::to_owned)
            .ok_or_else(|| invalid("artifact descriptor paths must be UTF-8"))
    };
    let dto = LeanWorkerPreparedHostArtifacts {
        protocol: 1,
        shim_manifest_path: path_string(prepared.manifest_path())?,
        shim_manifest_json: prepared.manifest_json().to_owned(),
        artifact_roots: prepared
            .artifact_roots()
            .iter()
            .map(|path| path_string(path))
            .collect::<io::Result<_>>()?,
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
    let mut stdout = std::io::stdout().lock();
    stdout.write_all(&bytes)?;
    stdout.write_all(b"\n")
}

pub(crate) fn from_environment() -> io::Result<LeanPreparedHostArtifacts> {
    let value = std::env::var(PREPARED_ENV).map_err(io::Error::other)?;
    if value.len() > MAX_DESCRIPTOR_BYTES {
        return Err(invalid("prepared descriptor exceeds 64 KiB"));
    }
    let dto: LeanWorkerPreparedHostArtifacts = serde_json::from_str(&value).map_err(io::Error::other)?;
    if dto.protocol != 1 {
        return Err(invalid("unsupported prepared artifact protocol"));
    }
    LeanPreparedHostArtifacts::from_snapshot(
        PathBuf::from(dto.shim_manifest_path),
        dto.shim_manifest_json,
        dto.artifact_roots.into_iter().map(PathBuf::from).collect(),
        dto.shim_search_paths.into_iter().map(PathBuf::from).collect(),
    )
    .map_err(io::Error::other)
}

pub(crate) struct ManifestSnapshot {
    path: PathBuf,
    json: Vec<u8>,
    parsed: CapabilityManifest,
}
impl ManifestSnapshot {
    fn read(path: &Path) -> io::Result<Self> {
        let path = path.canonicalize()?;
        let json = read_bounded(&path)?;
        Self::from_bytes(path, json)
    }
    fn from_bytes(path: PathBuf, json: Vec<u8>) -> io::Result<Self> {
        let parsed = CapabilityManifest::from_bytes(&path, &json).map_err(|error| invalid(error.message()))?;
        Ok(Self { path, json, parsed })
    }
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
    pub(crate) fn bytes(&self) -> &[u8] {
        &self.json
    }
    fn validate(&self) -> io::Result<()> {
        if read_bounded(&self.path)? != self.json {
            return Err(invalid("capability manifest changed before read admission"));
        }
        Ok(())
    }
    fn workspaces(&self) -> io::Result<Vec<PathBuf>> {
        std::iter::once(self.parsed.primary_dylib.as_path())
            .chain(
                self.parsed
                    .dependencies
                    .iter()
                    .map(lean_toolchain::LeanLibraryDependency::path_ref),
            )
            .map(artifact_workspace)
            .collect()
    }
}

/// No operation here loads user code. Capture -> custody/guardian ACK ->
/// revalidate -> exact-snapshot load is the only guarded construction path.
pub(crate) struct SessionAdmission {
    pub(crate) prepared: LeanPreparedHostArtifacts,
    pub(crate) capability: Option<ManifestSnapshot>,
    roots: Vec<PathBuf>,
    workspaces: Vec<PathBuf>,
    direct_library: Option<(PathBuf, PathBuf)>,
}
impl SessionAdmission {
    #[allow(
        clippy::wildcard_enum_match_arm,
        reason = "all future guarded mode variants must refuse before loading"
    )]
    pub(crate) fn capture(
        project_root: &str,
        mode: &HostSessionMode,
        prepared: LeanPreparedHostArtifacts,
    ) -> io::Result<Self> {
        let shim = ManifestSnapshot::from_bytes(
            prepared.manifest_path().to_path_buf(),
            prepared.manifest_json().as_bytes().to_vec(),
        )?;
        let shim_roots = project_artifact_root_union(&shim.workspaces()?)?;
        if shim_roots != prepared.artifact_roots() {
            return Err(invalid("prepared shim domains do not cover the exact manifest closure"));
        }
        for path in prepared.shim_search_paths() {
            let root = artifact_workspace(path)?;
            if !shim_roots.contains(&root) {
                return Err(invalid("prepared import path lies outside the shim domains"));
            }
        }
        let capability = match mode {
            HostSessionMode::Capability {
                manifest_path: Some(path),
                ..
            } => Some(ManifestSnapshot::read(Path::new(path))?),
            HostSessionMode::Capability {
                manifest_path: None, ..
            }
            | HostSessionMode::ShimsOnly => None,
            _ => return Err(invalid("unsupported guarded host mode")),
        };
        let direct_library = match mode {
            HostSessionMode::Capability {
                package,
                lib_name,
                manifest_path: None,
            } => {
                let extension = if cfg!(target_os = "macos") { "dylib" } else { "so" };
                let directory = Path::new(project_root).join(".lake/build/lib");
                let new = directory.join(format!("lib{}_{lib_name}.{extension}", package.replace('_', "__")));
                let old = directory.join(format!("lib{lib_name}.{extension}"));
                let path = if new.is_file() { new } else { old };
                let canonical = path.canonicalize()?;
                Some((path, canonical))
            }
            HostSessionMode::Capability {
                manifest_path: Some(_), ..
            }
            | HostSessionMode::ShimsOnly => None,
            _ => return Err(invalid("unsupported guarded host mode")),
        };
        let mut workspaces = BTreeSet::from([PathBuf::from(project_root)]);
        workspaces.extend(shim.workspaces()?);
        if let Some(capability) = &capability {
            workspaces.extend(capability.workspaces()?);
        }
        if let Some((_, canonical)) = &direct_library {
            workspaces.insert(artifact_workspace(canonical)?);
        }
        let workspaces: Vec<_> = workspaces.into_iter().collect();
        let roots = project_artifact_root_union(&workspaces)?;
        Ok(Self {
            prepared,
            capability,
            roots,
            workspaces,
            direct_library,
        })
    }
    pub(crate) fn roots(&self) -> &[PathBuf] {
        &self.roots
    }
    pub(crate) fn validate_after_admission(&self) -> io::Result<()> {
        if read_bounded(self.prepared.manifest_path())? != self.prepared.manifest_json().as_bytes() {
            return Err(invalid("prepared shim manifest changed before read admission"));
        }
        if let Some(capability) = &self.capability {
            capability.validate()?;
        }
        if let Some((path, canonical)) = &self.direct_library
            && path.canonicalize()? != *canonical
        {
            return Err(invalid("project capability changed domains before admission"));
        }
        if project_artifact_root_union(&self.workspaces)? != self.roots {
            return Err(invalid("Lake dependency domains changed before read admission"));
        }
        Ok(())
    }
}
fn read_bounded(path: &Path) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    File::open(path)?
        .take((MAX_DESCRIPTOR_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_DESCRIPTOR_BYTES {
        return Err(invalid("capability metadata exceeds 64 KiB"));
    }
    Ok(bytes)
}
fn artifact_workspace(path: &Path) -> io::Result<PathBuf> {
    let canonical = path.canonicalize()?;
    canonical
        .ancestors()
        .find(|ancestor| ancestor.file_name().is_some_and(|name| name == ".lake"))
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .ok_or_else(|| invalid("guarded capability artifact is outside a standard Lake output domain"))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::panic, clippy::indexing_slicing)]
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};
    static NONCE: AtomicU64 = AtomicU64::new(0);
    struct Fixture {
        directory: PathBuf,
        host: PathBuf,
        interop: PathBuf,
        project: PathBuf,
        semantic: PathBuf,
        dependency: PathBuf,
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
                fs::create_dir_all(root.join(".lake/build/lib/lean")).expect("output directory");
                fs::write(
                    root.join(".lake/build/lib/libcap.dylib"),
                    b"not loaded by metadata tests",
                )
                .expect("artifact");
                root
            };
            Self {
                host: root("host"),
                interop: root("interop"),
                project: root("project"),
                semantic: root("semantic"),
                dependency: root("dependency"),
                directory,
            }
        }
        fn manifest(root: &Path, package: &str, module: &str, deps: &[PathBuf]) -> PathBuf {
            let path = root.join(".lake/capability.json");
            fs::write(&path,serde_json::to_vec(&serde_json::json!({
                "schema_version":lean_toolchain::CAPABILITY_MANIFEST_SCHEMA_VERSION,
                "primary_dylib":root.join(".lake/build/lib/libcap.dylib"),"package":package,"module":module,
                "dependencies":deps.iter().map(|root|serde_json::json!({"dylib_path":root.join(".lake/build/lib/libcap.dylib")})).collect::<Vec<_>>(),"exports":[]
            })).expect("manifest JSON")).expect("manifest");
            path
        }
        fn prepared(&self, omit_interop: bool) -> LeanPreparedHostArtifacts {
            let manifest = Self::manifest(
                &self.host,
                "lean_rs_host_shims",
                "LeanRsHostShims",
                std::slice::from_ref(&self.interop),
            );
            let mut roots = vec![self.host.clone()];
            if !omit_interop {
                roots.push(self.interop.clone());
            }
            roots.sort();
            LeanPreparedHostArtifacts::from_snapshot(
                manifest.clone(),
                fs::read_to_string(manifest).expect("snapshot"),
                roots,
                vec![
                    self.interop.join(".lake/build/lib/lean"),
                    self.host.join(".lake/build/lib/lean"),
                ],
            )
            .expect("storage descriptor")
        }
        fn mode(&self) -> HostSessionMode {
            let manifest = Self::manifest(&self.semantic, "cap", "Cap", std::slice::from_ref(&self.dependency));
            HostSessionMode::Capability {
                package: "cap".to_owned(),
                lib_name: "Cap".to_owned(),
                manifest_path: Some(manifest.to_string_lossy().into_owned()),
            }
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _removed = fs::remove_dir_all(&self.directory);
        }
    }
    #[test]
    fn admission_includes_separate_semantic_primary_and_manifest_dependency() {
        let fixture = Fixture::new();
        let admission = SessionAdmission::capture(
            fixture.project.to_str().expect("UTF8"),
            &fixture.mode(),
            fixture.prepared(false),
        )
        .expect("complete admission");
        let mut expected = vec![
            fixture.host.clone(),
            fixture.interop.clone(),
            fixture.project.clone(),
            fixture.semantic.clone(),
            fixture.dependency.clone(),
        ];
        expected.sort();
        assert_eq!(admission.roots(), expected);
        admission.validate_after_admission().expect("unchanged metadata");
    }
    #[test]
    fn admission_rejects_descriptor_omitting_shim_dependency_before_loading() {
        let fixture = Fixture::new();
        let result = SessionAdmission::capture(
            fixture.project.to_str().expect("UTF8"),
            &HostSessionMode::ShimsOnly,
            fixture.prepared(true),
        );
        assert!(
            result
                .err()
                .expect("must refuse")
                .to_string()
                .contains("exact manifest closure")
        );
    }
    #[test]
    fn admission_refuses_manifest_changed_while_waiting_for_custody() {
        let fixture = Fixture::new();
        let mode = fixture.mode();
        let admission =
            SessionAdmission::capture(fixture.project.to_str().expect("UTF8"), &mode, fixture.prepared(false))
                .expect("capture");
        Fixture::manifest(&fixture.semantic, "cap", "Cap", &[]);
        assert!(
            admission
                .validate_after_admission()
                .expect_err("changed snapshot must refuse")
                .to_string()
                .contains("manifest changed")
        );
    }
    #[test]
    fn admission_refuses_workspace_dependency_added_before_custody() {
        let fixture = Fixture::new();
        let admission = SessionAdmission::capture(
            fixture.project.to_str().expect("UTF8"),
            &HostSessionMode::ShimsOnly,
            fixture.prepared(false),
        )
        .expect("capture");
        fs::write(
            fixture.dependency.join("lake-manifest.json"),
            r#"{"version":"1.1.0","packages":[]}"#,
        )
        .expect("dependency manifest");
        fs::write(fixture.project.join("lake-manifest.json"),serde_json::to_vec(&serde_json::json!({"version":"1.1.0","packages":[{"name":"dep","type":"path","dir":fixture.dependency,"manifestFile":"lake-manifest.json"}]})).expect("graph")).expect("project graph");
        assert!(
            admission
                .validate_after_admission()
                .expect_err("new domain must refuse")
                .to_string()
                .contains("dependency domains changed")
        );
    }
}
