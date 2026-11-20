//! Runtime-free preparation and complete, captured pre-load admission metadata.
use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::fs::File;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use lean_rs_host::host::{
    LeanPreparedHostArtifacts, prepare_host_artifacts, ready_host_artifacts_for_generated_output_with_ipc_root,
    ready_host_artifacts_with_ipc_root,
};
use lean_rs_worker_protocol::protocol::HostSessionMode;
use lean_rs_worker_protocol::types::LeanWorkerPreparedHostArtifacts;
use lean_toolchain::CapabilityManifest;
use lean_toolchain::shared_writer::{SHARED_WRITER_EXECUTABLE_ENV, project_artifact_roots_with_domains};

const MAX_DESCRIPTOR_BYTES: usize = 64 * 1024;
pub(crate) const PREPARED_ENV: &str = "LEAN_RS_PREPARED_HOST_ARTIFACTS";

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

pub(crate) fn preparation_mode() -> Option<ExitCode> {
    let arg = std::env::args_os().nth(1)?;
    let (probe, generated) = if arg == "--lean-rs-ready-host-artifacts" {
        (true, false)
    } else if arg == "--lean-rs-ready-host-artifacts-generated" {
        (true, true)
    } else if arg == "--lean-rs-prepare-host-artifacts" {
        (false, false)
    } else {
        return None;
    };
    let result = (|| -> Result<Option<LeanPreparedHostArtifacts>, Box<dyn std::error::Error>> {
        let args = std::env::args_os().collect::<Vec<_>>();
        let valid_args = if generated {
            args.len() == 4
        } else if probe {
            args.len() == 2 || args.len() == 3
        } else {
            args.len() == 2
        };
        if !valid_args || std::env::var_os(SHARED_WRITER_EXECUTABLE_ENV).is_none() {
            return Err(
                invalid("artifact preparation requires its sole mode flag and an explicit writer helper").into(),
            );
        }
        // This branch precedes all runtime, parent-watchdog and native ownership
        // initialization. Any actual Lake work is owned by the configured H.
        if probe {
            if generated {
                let [_, _, ipc_root, output_key] = args.as_slice() else {
                    return Err(invalid(
                        "artifact preparation requires its sole mode flag and an explicit writer helper",
                    )
                    .into());
                };
                Ok(ready_host_artifacts_for_generated_output_with_ipc_root(
                    output_key
                        .to_str()
                        .ok_or_else(|| invalid("generated output key must be UTF-8"))?,
                    Path::new(ipc_root),
                )?
                .map(LeanPreparedHostArtifacts::with_project_generated_output_matching_shims)
                .transpose()?)
            } else if let Some(ipc_root) = args.get(2) {
                Ok(ready_host_artifacts_with_ipc_root(Path::new(ipc_root))?)
            } else {
                Ok(lean_rs_host::host::ready_host_artifacts()?)
            }
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
        protocol: if prepared.project_uses_generated_output() { 2 } else { 1 },
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
    Ok(bytes)
}

pub(crate) fn from_launch(value: Option<&OsStr>, shared_reads: bool) -> io::Result<Option<LeanPreparedHostArtifacts>> {
    let Some(value) = value else {
        return if shared_reads {
            Err(invalid("shared artifact reads require a prepared host descriptor"))
        } else {
            Ok(None)
        };
    };
    let value = value
        .to_str()
        .ok_or_else(|| invalid("prepared descriptor must be UTF-8"))?;
    if value.len() > MAX_DESCRIPTOR_BYTES {
        return Err(invalid("prepared descriptor exceeds 64 KiB"));
    }
    let dto: LeanWorkerPreparedHostArtifacts = serde_json::from_str(value).map_err(io::Error::other)?;
    if dto.protocol != 1 && dto.protocol != 2 {
        return Err(invalid("unsupported prepared artifact protocol"));
    }
    let prepared = LeanPreparedHostArtifacts::from_snapshot(
        PathBuf::from(dto.shim_manifest_path),
        dto.shim_manifest_json,
        dto.artifact_roots.into_iter().map(PathBuf::from).collect(),
        dto.shim_search_paths.into_iter().map(PathBuf::from).collect(),
    )
    .map_err(io::Error::other)?;
    if dto.protocol == 2 {
        let generated = prepared
            .clone()
            .with_project_generated_output_matching_shims()
            .map_err(io::Error::other)?;
        if shared_reads {
            return Ok(Some(generated));
        }
        // Ordinary project imports are selected by WorkerAuthority::use_generated_project_imports.
    }
    if !shared_reads {
        let path = prepared.manifest_path();
        if path.canonicalize()? != path || read_bounded(path)? != prepared.manifest_json().as_bytes() {
            return Err(invalid(
                "ordinary prepared host manifest differs from its launch snapshot",
            ));
        }
    }
    Ok(Some(prepared))
}

pub(crate) struct ManifestSnapshot {
    path: PathBuf,
    json: Vec<u8>,
    parsed: CapabilityManifest,
    primary_workspace: PathBuf,
    artifact_domains: Vec<PathBuf>,
}
impl ManifestSnapshot {
    fn read(path: &Path) -> io::Result<Self> {
        let path = path.canonicalize()?;
        let json = read_bounded(&path)?;
        Self::from_bytes(path, json)
    }
    fn from_bytes(path: PathBuf, json: Vec<u8>) -> io::Result<Self> {
        let parsed = CapabilityManifest::from_bytes(&path, &json).map_err(|error| invalid(error.message()))?;
        let primary_workspace = artifact_workspace(&parsed.primary_dylib)?;
        let artifact_domains = Self::resolve_artifact_domains(&parsed)?;
        Ok(Self {
            path,
            json,
            parsed,
            primary_workspace,
            artifact_domains,
        })
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
        self.validate_domains()
    }
    fn validate_domains(&self) -> io::Result<()> {
        if artifact_workspace(&self.parsed.primary_dylib)? != self.primary_workspace
            || Self::resolve_artifact_domains(&self.parsed)? != self.artifact_domains
        {
            return Err(invalid("capability artifact domains changed before read admission"));
        }
        Ok(())
    }
    fn primary_workspace(&self) -> &Path {
        &self.primary_workspace
    }
    fn artifact_domains(&self) -> &[PathBuf] {
        &self.artifact_domains
    }
    // Library dependencies identify domains to protect, not independently
    // runnable Lake workspaces. Their names resolve in a seed's flat map.
    fn resolve_artifact_domains(parsed: &CapabilityManifest) -> io::Result<Vec<PathBuf>> {
        let domains = std::iter::once(parsed.primary_dylib.as_path())
            .chain(
                parsed
                    .dependencies
                    .iter()
                    .map(lean_toolchain::LeanLibraryDependency::path_ref),
            )
            .map(artifact_workspace)
            .collect::<io::Result<BTreeSet<_>>>()?;
        Ok(domains.into_iter().collect())
    }
}

/// No operation here loads user code. Capture -> custody/guardian ACK ->
/// revalidate -> exact-snapshot load is the only guarded construction path.
pub(crate) struct SessionAdmission {
    pub(crate) prepared: LeanPreparedHostArtifacts,
    pub(crate) capability: Option<ManifestSnapshot>,
    shim: ManifestSnapshot,
    shim_workspace_seeds: Vec<PathBuf>,
    roots: Vec<PathBuf>,
    workspace_seeds: Vec<PathBuf>,
    frozen_domains: Vec<PathBuf>,
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
        // The two bundled shim packages are independent build workspaces.
        // Resolve those roots from their import paths, never from the DTO's
        // claimed artifact union or from every dependency DLL in its manifest.
        let shim_seeds = prepared
            .shim_search_paths()
            .iter()
            .map(|path| artifact_workspace(path))
            .collect::<io::Result<Vec<_>>>()?;
        if shim_seeds.len() != 2
            || shim_seeds.iter().collect::<BTreeSet<_>>().len() != 2
            || !shim_seeds.iter().any(|root| root == shim.primary_workspace())
        {
            return Err(invalid(
                "shim primary workspace requires two distinct import workspace seeds",
            ));
        }
        let shim_roots = project_artifact_roots_with_domains(&shim_seeds, shim.artifact_domains())?;
        if shim_roots != prepared.artifact_roots() {
            return Err(invalid("prepared shim domains do not cover the exact manifest closure"));
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
        let project = Path::new(project_root).canonicalize()?;
        let project_roots = project_artifact_roots_with_domains(std::slice::from_ref(&project), &[])?;
        let mut workspace_seeds = BTreeSet::from([project]);
        workspace_seeds.extend(shim_seeds.iter().cloned());
        let mut frozen_domains: BTreeSet<_> = shim_roots.into_iter().collect();
        if let Some(capability) = &capability {
            let primary = capability.primary_workspace().to_path_buf();
            // A capability package already resolved through the project map
            // cannot be reinterpreted as a standalone Lake workspace: its own
            // dependency names still belong to that project's flattened map.
            if !project_roots.contains(&primary) {
                workspace_seeds.insert(primary);
            }
            frozen_domains.extend(capability.artifact_domains().iter().cloned());
        }
        if let Some((_, canonical)) = &direct_library {
            frozen_domains.insert(artifact_workspace(canonical)?);
        }
        let workspace_seeds: Vec<_> = workspace_seeds.into_iter().collect();
        let frozen_domains: Vec<_> = frozen_domains.into_iter().collect();
        let roots = project_artifact_roots_with_domains(&workspace_seeds, &frozen_domains)?;
        Ok(Self {
            prepared,
            capability,
            shim,
            shim_workspace_seeds: shim_seeds,
            roots,
            workspace_seeds,
            frozen_domains,
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
        self.shim.validate_domains()?;
        let shim_seeds = self
            .prepared
            .shim_search_paths()
            .iter()
            .map(|path| artifact_workspace(path))
            .collect::<io::Result<Vec<_>>>()?;
        if shim_seeds != self.shim_workspace_seeds {
            return Err(invalid("prepared shim import domains changed before read admission"));
        }
        if let Some(capability) = &self.capability {
            capability.validate()?;
        }
        if let Some((path, canonical)) = &self.direct_library
            && path.canonicalize()? != *canonical
        {
            return Err(invalid("project capability changed domains before admission"));
        }
        if project_artifact_roots_with_domains(&self.workspace_seeds, &self.frozen_domains)? != self.roots {
            return Err(invalid("Lake dependency domains changed before read admission"));
        }
        Ok(())
    }
}
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
        fn output_root(root: PathBuf) -> PathBuf {
            fs::create_dir_all(root.join(".lake/build/lib/lean")).expect("output directory");
            fs::write(root.join(".lake/build/lib/libcap.dylib"), b"metadata fixture").expect("artifact");
            root
        }
        fn lake_manifest(root: &Path, names: &[&str]) {
            let packages = names
                .iter()
                .map(|name| {
                    serde_json::json!({
                        "name":name, "type":"git", "subDir":null,
                        "manifestFile":"lake-manifest.json"
                    })
                })
                .collect::<Vec<_>>();
            fs::write(
                root.join("lake-manifest.json"),
                serde_json::to_vec(&serde_json::json!({
                    "version":"1.3.0", "packagesDir":".lake/packages", "packages":packages
                }))
                .expect("Lake graph JSON"),
            )
            .expect("Lake graph");
        }
        fn flat_graph(root: &Path) -> (PathBuf, PathBuf) {
            let t = Self::output_root(root.join(".lake/packages/T"));
            let u = Self::output_root(root.join(".lake/packages/U"));
            Self::lake_manifest(root, &["T", "U"]);
            Self::lake_manifest(&t, &["U"]);
            Self::lake_manifest(&u, &[]);
            assert!(!t.join(".lake/packages").exists());
            (t, u)
        }
        fn prepared_flat_shims(&self, t: &Path, u: &Path) -> LeanPreparedHostArtifacts {
            let manifest = Self::manifest(
                &self.host,
                "lean_rs_host_shims",
                "LeanRsHostShims",
                &[self.interop.clone(), t.to_path_buf()],
            );
            let mut roots = vec![
                self.host.clone(),
                self.interop.clone(),
                t.to_path_buf(),
                u.to_path_buf(),
            ];
            roots.sort();
            LeanPreparedHostArtifacts::from_snapshot(
                manifest.clone(),
                fs::read_to_string(manifest).expect("shim snapshot"),
                roots,
                vec![
                    self.interop.join(".lake/build/lib/lean"),
                    self.host.join(".lake/build/lib/lean"),
                ],
            )
            .expect("flat shim descriptor")
        }
        fn mode_at(primary: &Path, dependencies: &[PathBuf]) -> HostSessionMode {
            let manifest = Self::manifest(primary, "cap", "Cap", dependencies);
            HostSessionMode::Capability {
                package: "cap".to_owned(),
                lib_name: "Cap".to_owned(),
                manifest_path: Some(manifest.to_string_lossy().into_owned()),
            }
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
    fn ordinary_launch_selects_the_exact_prepared_snapshot_or_refuses() {
        use std::os::unix::ffi::OsStrExt as _;

        let fixture = Fixture::new();
        let expected = fixture.prepared(false);
        let bytes = descriptor_bytes(&expected).expect("encode deployment outside bundled roots");
        let explicit = Some(OsStr::from_bytes(&bytes));
        let actual = from_launch(explicit, false)
            .expect("ordinary launch")
            .expect("explicit snapshot");
        assert_eq!(actual.manifest_path(), expected.manifest_path());
        assert_eq!(actual.manifest_json(), expected.manifest_json());
        assert_eq!(actual.shim_search_paths(), expected.shim_search_paths());
        assert_eq!(actual.artifact_roots(), expected.artifact_roots());
        assert!(from_launch(None, false).expect("ordinary default").is_none());
        assert!(from_launch(None, true).is_err());
        assert!(from_launch(Some(OsStr::new("")), false).is_err());
        assert!(from_launch(Some(OsStr::from_bytes(&[0xff])), false).is_err());

        fs::write(expected.manifest_path(), b"{}").expect("replace named manifest");
        assert!(
            from_launch(explicit, false)
                .expect_err("stale snapshot")
                .to_string()
                .contains("differs")
        );
        fs::remove_file(expected.manifest_path()).expect("remove named manifest");
        assert_eq!(
            from_launch(explicit, false).expect_err("missing manifest").kind(),
            io::ErrorKind::NotFound
        );
    }

    #[test]
    fn ordinary_descriptor_cannot_select_generated_project_imports() {
        use std::os::unix::ffi::OsStrExt as _;

        let fixture = Fixture::new();
        let flat = fixture.prepared(false);
        let paths = [&fixture.interop, &fixture.host].map(|root| {
            let path = root.join(".lake/lean-rs-generated/lean-4.36.0/lib/lean");
            fs::create_dir_all(&path).expect("generated shim path");
            path
        });
        let generated = LeanPreparedHostArtifacts::from_snapshot(
            flat.manifest_path().to_owned(),
            flat.manifest_json().to_owned(),
            flat.artifact_roots().to_vec(),
            paths.to_vec(),
        )
        .expect("generated shim descriptor")
        .with_project_generated_output_matching_shims()
        .expect("protocol 2");
        let bytes = descriptor_bytes(&generated).expect("encode generated descriptor");
        let explicit = Some(OsStr::from_bytes(&bytes));
        let ordinary = from_launch(explicit, false)
            .expect("ordinary launch")
            .expect("explicit snapshot");
        assert_eq!(ordinary.shim_search_paths(), paths);
        assert!(!ordinary.project_uses_generated_output());
        assert!(
            from_launch(explicit, true)
                .expect("shared launch")
                .expect("required snapshot")
                .project_uses_generated_output()
        );
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
    #[test]
    fn separate_capability_uses_its_flat_workspace_map_for_dependency_dlls() {
        let fixture = Fixture::new();
        let (t, u) = Fixture::flat_graph(&fixture.semantic);
        let mode = Fixture::mode_at(&fixture.semantic, std::slice::from_ref(&t));
        let admission =
            SessionAdmission::capture(fixture.project.to_str().expect("UTF8"), &mode, fixture.prepared(false))
                .expect("dependency DLL is a protected domain, not a workspace seed");
        let expected = BTreeSet::from([
            fixture.project.clone(),
            fixture.host.clone(),
            fixture.interop.clone(),
            fixture.semantic.clone(),
            t.clone(),
            u,
        ])
        .into_iter()
        .collect::<Vec<_>>();
        assert_eq!(admission.roots(), expected);
        admission.validate_after_admission().expect("same flat graph after ACK");
        assert!(
            !t.join(".lake/packages").exists(),
            "no standalone package materialization"
        );
    }
    #[test]
    fn shim_dependencies_use_only_the_two_independent_shim_workspace_maps() {
        let fixture = Fixture::new();
        let (t, u) = Fixture::flat_graph(&fixture.host);
        let admission = SessionAdmission::capture(
            fixture.project.to_str().expect("UTF8"),
            &HostSessionMode::ShimsOnly,
            fixture.prepared_flat_shims(&t, &u),
        )
        .expect("shim dependency DLL must not be expanded as a standalone workspace");
        let expected = BTreeSet::from([
            fixture.project.clone(),
            fixture.host.clone(),
            fixture.interop.clone(),
            t.clone(),
            u,
        ])
        .into_iter()
        .collect::<Vec<_>>();
        assert_eq!(admission.roots(), expected);
        admission.validate_after_admission().expect("same shim graph after ACK");
        assert!(!t.join(".lake/packages").exists());
    }
    #[test]
    fn capability_primary_already_in_project_map_is_not_reexpanded() {
        let fixture = Fixture::new();
        let (t, u) = Fixture::flat_graph(&fixture.project);
        let mode = Fixture::mode_at(&t, std::slice::from_ref(&u));
        let admission =
            SessionAdmission::capture(fixture.project.to_str().expect("UTF8"), &mode, fixture.prepared(false))
                .expect("capability primary resolves in its containing project map");
        let expected = BTreeSet::from([
            fixture.project.clone(),
            fixture.host.clone(),
            fixture.interop.clone(),
            t.clone(),
            u,
        ])
        .into_iter()
        .collect::<Vec<_>>();
        assert_eq!(admission.roots(), expected);
        admission
            .validate_after_admission()
            .expect("project map remains stable");
        assert!(!t.join(".lake/packages").exists());
    }
    #[test]
    fn frozen_dependency_domain_does_not_discover_an_unrelated_workspace_graph() {
        let fixture = Fixture::new();
        fs::write(fixture.dependency.join("lake-manifest.json"), b"not a workspace graph").expect("unrelated metadata");
        let admission = SessionAdmission::capture(
            fixture.project.to_str().expect("UTF8"),
            &fixture.mode(),
            fixture.prepared(false),
        )
        .expect("captured DLL domains do not authorize unrelated graph discovery");
        assert!(admission.roots().contains(&fixture.dependency));
        admission
            .validate_after_admission()
            .expect("immutable frozen domain remains protected");
    }
    #[test]
    fn added_domain_in_captured_capability_workspace_refuses_after_ack() {
        let fixture = Fixture::new();
        let (t, _u) = Fixture::flat_graph(&fixture.semantic);
        let mode = Fixture::mode_at(&fixture.semantic, std::slice::from_ref(&t));
        let admission =
            SessionAdmission::capture(fixture.project.to_str().expect("UTF8"), &mode, fixture.prepared(false))
                .expect("capture capability graph");
        let v = Fixture::output_root(fixture.semantic.join(".lake/packages/V"));
        Fixture::lake_manifest(&v, &[]);
        Fixture::lake_manifest(&fixture.semantic, &["T", "U", "V"]);
        assert!(
            admission
                .validate_after_admission()
                .expect_err("no union extension after ACK")
                .to_string()
                .contains("dependency domains changed")
        );
        assert!(!admission.roots().contains(&v));
    }
    #[test]
    fn added_domain_in_captured_shim_workspace_refuses_after_ack() {
        let fixture = Fixture::new();
        let (t, u) = Fixture::flat_graph(&fixture.host);
        let admission = SessionAdmission::capture(
            fixture.project.to_str().expect("UTF8"),
            &HostSessionMode::ShimsOnly,
            fixture.prepared_flat_shims(&t, &u),
        )
        .expect("capture shim graph");
        let v = Fixture::output_root(fixture.host.join(".lake/packages/V"));
        Fixture::lake_manifest(&v, &[]);
        Fixture::lake_manifest(&fixture.host, &["T", "U", "V"]);
        assert!(
            admission
                .validate_after_admission()
                .expect_err("no shim union extension after ACK")
                .to_string()
                .contains("dependency domains changed")
        );
        assert!(!admission.roots().contains(&v));
    }
    #[test]
    fn descriptor_cannot_self_authorize_an_extra_shim_domain() {
        let fixture = Fixture::new();
        let prepared = fixture.prepared(false);
        let mut roots = prepared.artifact_roots().to_vec();
        roots.push(fixture.dependency.clone());
        roots.sort();
        let prepared = LeanPreparedHostArtifacts::from_snapshot(
            prepared.manifest_path().to_path_buf(),
            prepared.manifest_json().to_owned(),
            roots,
            prepared.shim_search_paths().to_vec(),
        )
        .expect("storage descriptor alone does not prove admission coverage");
        assert!(
            SessionAdmission::capture(
                fixture.project.to_str().expect("UTF8"),
                &HostSessionMode::ShimsOnly,
                prepared,
            )
            .err()
            .expect("extra root must refuse")
            .to_string()
            .contains("exact manifest closure")
        );
    }
    #[cfg(unix)]
    #[test]
    fn retargeted_capability_dependency_cannot_extend_frozen_domains_after_ack() {
        let fixture = Fixture::new();
        let admission = SessionAdmission::capture(
            fixture.project.to_str().expect("UTF8"),
            &fixture.mode(),
            fixture.prepared(false),
        )
        .expect("capture dependency artifact identity");
        let outside = Fixture::output_root(fixture.directory.join("outside"));
        let dependency = fixture.dependency.join(".lake/build/lib/libcap.dylib");
        fs::remove_file(&dependency).expect("replace only fixture DLL");
        std::os::unix::fs::symlink(outside.join(".lake/build/lib/libcap.dylib"), dependency)
            .expect("retarget dependency alias");
        assert!(
            admission
                .validate_after_admission()
                .expect_err("changed DLL domain must refuse")
                .to_string()
                .contains("artifact domains changed")
        );
        assert!(!admission.roots().contains(&outside));
    }
    #[cfg(unix)]
    #[test]
    fn retargeted_shim_dependency_cannot_extend_frozen_domains_after_ack() {
        let fixture = Fixture::new();
        let admission = SessionAdmission::capture(
            fixture.project.to_str().expect("UTF8"),
            &HostSessionMode::ShimsOnly,
            fixture.prepared(false),
        )
        .expect("capture shim dependency identity");
        let outside = Fixture::output_root(fixture.directory.join("outside"));
        let dependency = fixture.interop.join(".lake/build/lib/libcap.dylib");
        fs::remove_file(&dependency).expect("replace only fixture DLL");
        std::os::unix::fs::symlink(outside.join(".lake/build/lib/libcap.dylib"), dependency)
            .expect("retarget shim dependency alias");
        assert!(
            admission
                .validate_after_admission()
                .expect_err("changed shim DLL domain must refuse")
                .to_string()
                .contains("artifact domains changed")
        );
        assert!(!admission.roots().contains(&outside));
    }
    #[cfg(unix)]
    #[test]
    fn retargeted_shim_import_path_cannot_extend_frozen_domains_after_ack() {
        let fixture = Fixture::new();
        let admission = SessionAdmission::capture(
            fixture.project.to_str().expect("UTF8"),
            &HostSessionMode::ShimsOnly,
            fixture.prepared(false),
        )
        .expect("capture shim import domain");
        let outside = Fixture::output_root(fixture.directory.join("outside"));
        let import = fixture.interop.join(".lake/build/lib/lean");
        fs::remove_dir(&import).expect("replace only empty fixture import directory");
        std::os::unix::fs::symlink(outside.join(".lake/build/lib/lean"), import).expect("retarget shim import alias");
        assert!(
            admission
                .validate_after_admission()
                .expect_err("changed import domain must refuse")
                .to_string()
                .contains("shim import domains changed")
        );
        assert!(!admission.roots().contains(&outside));
    }
    #[test]
    fn descriptor_cannot_omit_the_shim_primary_workspace_seed_and_its_graph() {
        let fixture = Fixture::new();
        let _graph = Fixture::flat_graph(&fixture.host);
        let prepared = fixture.prepared(false);
        let mut roots = vec![
            fixture.host.clone(),
            fixture.interop.clone(),
            fixture.dependency.clone(),
        ];
        roots.sort();
        let prepared = LeanPreparedHostArtifacts::from_snapshot(
            prepared.manifest_path().to_path_buf(),
            prepared.manifest_json().to_owned(),
            roots,
            vec![
                fixture.interop.join(".lake/build/lib/lean"),
                fixture.dependency.join(".lake/build/lib/lean"),
            ],
        )
        .expect("storage descriptor must still establish independent seeds");
        assert!(
            SessionAdmission::capture(
                fixture.project.to_str().expect("UTF8"),
                &HostSessionMode::ShimsOnly,
                prepared,
            )
            .err()
            .expect("missing primary seed must refuse")
            .to_string()
            .contains("shim primary workspace")
        );
    }
    #[test]
    fn descriptor_cannot_replace_the_second_shim_workspace_with_a_duplicate() {
        let fixture = Fixture::new();
        let prepared = fixture.prepared(false);
        let prepared = LeanPreparedHostArtifacts::from_snapshot(
            prepared.manifest_path().to_path_buf(),
            prepared.manifest_json().to_owned(),
            prepared.artifact_roots().to_vec(),
            vec![
                fixture.host.join(".lake/build/lib/lean"),
                fixture.host.join(".lake/build/lib/lean"),
            ],
        )
        .expect("storage descriptor must still establish distinct seeds");
        assert!(
            SessionAdmission::capture(
                fixture.project.to_str().expect("UTF8"),
                &HostSessionMode::ShimsOnly,
                prepared,
            )
            .err()
            .expect("duplicate seed must refuse")
            .to_string()
            .contains("two distinct import workspace seeds")
        );
    }
}
