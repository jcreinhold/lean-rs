//! Lake project discovery helper.
//!
//! [`LakeProject`] resolves the on-disk layout Lake produces for a Lean
//! package: where the compiled `.dylib`/`.so` for a capability library
//! lives, where the `.olean` files an imported module needs reside, and
//! where the bundled `lean-rs-host-shims` and `lean-rs-interop-shims`
//! packages compile their dylibs so the host stack can load them
//! alongside the user's capability dylib. The layouts are stable
//! across the supported toolchain range; paths are built
//! by concatenation and the bundled shims are built on demand through
//! `lean-toolchain`.
//!
//! The type is `pub(crate)`—`LeanHost` exposes the only operations
//! callers actually want (open the project, load a capability dylib).

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use lean_rs::error::LeanResult;
use lean_toolchain::{BuiltLeanCapability, LeanExportSignature};
use serde_json::Value;

/// Lake package name the host shim contract ships under.
pub(crate) const SHIM_PACKAGE_NAME: &str = "lean_rs_host_shims";
/// Lake `lean_lib` name inside the shim package. Constant on our side
/// because the shim package is ours; consumers don't re-declare it.
pub(crate) const SHIM_LIB_NAME: &str = "LeanRsHostShims";
/// Lake `lean_lib` name inside the generic interop shim package.
pub(crate) const INTEROP_LIB_NAME: &str = "LeanRsInterop";

const BUNDLED_SHIMS_DIR: &str = "shims";
const BUNDLED_HOST_SHIMS_DIR: &str = "lean-rs-host-shims";
const BUNDLED_INTEROP_SHIMS_DIR: &str = "lean-rs-interop-shims";

static BUNDLED_SHIM_BUILD_LOCK: Mutex<()> = Mutex::new(());

/// A validated Lake project root.
///
/// Owned by [`crate::host::LeanHost`]; never escapes the `host` module.
pub(crate) struct LakeProject {
    root: PathBuf,
}

impl LakeProject {
    /// Bind a `LakeProject` to the given directory.
    ///
    /// # Errors
    ///
    /// Returns [`lean_rs::LeanError::Host`] with code
    /// [`lean_rs::LeanDiagnosticCode::ModuleInit`] if the path does not
    /// exist or is not a directory. Diagnostic embeds the requested
    /// path.
    pub(crate) fn new(root: impl AsRef<Path>) -> LeanResult<Self> {
        let root = root.as_ref();
        if !root.is_dir() {
            return Err(lean_rs::__host_internals::host_module_init(format!(
                "Lake project root '{}' does not exist or is not a directory",
                root.display()
            )));
        }
        let root = fs::canonicalize(root).map_err(|err| {
            lean_rs::__host_internals::host_module_init(format!(
                "Lake project root '{}' could not be canonicalized: {err}",
                root.display()
            ))
        })?;
        Ok(Self { root })
    }

    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    /// Canonical shared output domains used by this host and its bundled shims.
    ///
    /// Keep this with the owning root functions when bundled outputs move.
    pub(crate) fn artifact_roots(&self) -> LeanResult<Vec<PathBuf>> {
        lean_toolchain::shared_writer::project_artifact_roots(&self.root)
            .map_err(|error| lean_rs::__host_internals::host_module_init(error.to_string()))
    }

    /// On-disk path to the compiled capability dylib for the
    /// `(package, lean_lib_name)` pair.
    ///
    /// The on-disk path Lake materialises the consumer's `lean_lib` to,
    /// resolved by probing both supported naming conventions.
    ///
    /// Lake's shared-library filename changed between Lean 4.26 and 4.27:
    /// older versions emit `.lake/build/lib/lib{lib_name}.{dylib,so}` (just
    /// the library name); 4.27+ emit
    /// `.lake/build/lib/lib{escaped_package}_{lib_name}.{dylib,so}` where
    /// `escaped_package` doubles every underscore. Both conventions are
    /// part of the supported window (see `docs/version-matrix.md`);
    /// this method returns
    /// whichever candidate exists so the Rust loader is naming-convention-
    /// agnostic. Returns the new-style path as a fallback for diagnostics
    /// when neither candidate exists on disk; the caller surfaces the
    /// failure as an [`crate::error::ModuleInit`] error.
    pub(crate) fn capability_dylib(&self, package: &str, lib_name: &str) -> PathBuf {
        let dylib_extension = if cfg!(target_os = "macos") { "dylib" } else { "so" };
        let lib_dir = self.root.join(".lake").join("build").join("lib");
        let escaped_package = package.replace('_', "__");
        let new_style = lib_dir.join(format!("lib{escaped_package}_{lib_name}.{dylib_extension}"));
        let old_style = lib_dir.join(format!("lib{lib_name}.{dylib_extension}"));
        if new_style.is_file() {
            new_style
        } else if old_style.is_file() {
            old_style
        } else {
            new_style
        }
    }

    /// Search path the Lean side passes to `Lean.initSearchPath` so
    /// `Lean.importModules` can locate the `.olean` files Lake built for
    /// this project.
    pub(crate) fn olean_search_path(&self) -> PathBuf {
        project_olean_search_path(&self.root)
    }

    /// Search paths the Lean side passes to `Lean.initSearchPath` so
    /// `Lean.importModules` can locate the `.olean` files Lake built for
    /// this project and every package recorded in `lake-manifest.json`.
    ///
    /// Missing or malformed manifest data degrades to the project-local
    /// entry, matching the layout used by dependency-free Lake projects.
    pub(crate) fn olean_search_paths(&self) -> Vec<PathBuf> {
        let mut paths = vec![self.olean_search_path()];
        let manifest_path = self.root.join("lake-manifest.json");
        let Ok(manifest_bytes) = fs::read(manifest_path) else {
            return paths;
        };
        let Ok(manifest) = serde_json::from_slice::<Value>(&manifest_bytes) else {
            return paths;
        };
        let packages_dir = manifest
            .get("packagesDir")
            .and_then(Value::as_str)
            .unwrap_or(".lake/packages");
        let Some(packages) = manifest.get("packages").and_then(Value::as_array) else {
            return paths;
        };

        for package in packages {
            if let Some(package_root) = manifest_package_root(&self.root, packages_dir, package) {
                paths.push(project_olean_search_path(&package_root));
            }
        }
        paths
    }

    /// Source roots passed to the source-range shim.
    ///
    /// Lean's declaration-range extension stores positions, not a stable
    /// absolute path. The host keeps Lake layout knowledge here and lets
    /// the Lean shim resolve a declaration's module against these roots
    /// when it needs a user-facing file label.
    pub(crate) fn source_roots(&self) -> LeanResult<Vec<PathBuf>> {
        Ok(vec![self.root.clone(), bundled_host_shims_root()])
    }

    /// Build the bundled host-shim target and emit a manifest carrying
    /// trusted export signatures.
    pub(crate) fn shim_capability(export_signatures: Vec<LeanExportSignature>) -> LeanResult<BuiltLeanCapability> {
        // Both independently runnable packages need their own validity keys.
        // The host target can rebuild the path dependency, so its writer graph
        // also includes the interop domain rather than nesting another writer.
        let root = bundled_interop_shims_root();
        let bootstrap =
            lean_toolchain::shared_writer::PrivateLakeBootstrap::new(Path::new(env!("CARGO_MANIFEST_DIR")), &root)
                .map_err(artifact_error)?;
        drop(
            interop_capability_builder(&root)
                .private_bootstrap(bootstrap)
                .build_quiet()
                .map_err(|error| artifact_error(std::io::Error::other(error)))?,
        );
        build_bundled_capability(&bundled_host_shims_root(), SHIM_LIB_NAME, export_signatures)
    }
}

fn project_olean_search_path(project_root: &Path) -> PathBuf {
    project_root.join(".lake").join("build").join("lib").join("lean")
}

fn manifest_package_root(project_root: &Path, packages_dir: &str, package: &Value) -> Option<PathBuf> {
    let package_name = package
        .get("name")
        .and_then(Value::as_str)
        .map(normalize_lake_identifier)?;
    if package.get("type").and_then(Value::as_str) == Some("path")
        && let Some(dir) = package.get("dir").and_then(Value::as_str)
    {
        return Some(project_root.join(dir));
    }
    // A git dependency is checked out to `<packagesDir>/<name>`, but when the
    // Lake package lives in a subdirectory of that repo (the `require ... / "sub"`
    // form), Lake records the subpath as `subDir` and the package root — hence
    // its `.lake/build` tree — is nested under it. Dropping `subDir` points the
    // search path at the repo root instead of the package, so its `.olean`
    // files become unreachable.
    let mut package_root = project_root.join(packages_dir).join(&package_name);
    if let Some(sub_dir) = package.get("subDir").and_then(Value::as_str) {
        package_root.push(sub_dir);
    }
    Some(package_root)
}

fn normalize_lake_identifier(raw: &str) -> String {
    raw.trim()
        .trim_matches('`')
        .trim_matches('«')
        .trim_matches('»')
        .trim_matches('"')
        .trim()
        .to_owned()
}

fn bundled_host_shims_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join(BUNDLED_SHIMS_DIR)
        .join(BUNDLED_HOST_SHIMS_DIR)
}

fn bundled_interop_shims_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join(BUNDLED_SHIMS_DIR)
        .join(BUNDLED_INTEROP_SHIMS_DIR)
}

fn build_bundled_capability(
    project_root: &Path,
    target_name: &str,
    export_signatures: Vec<LeanExportSignature>,
) -> LeanResult<BuiltLeanCapability> {
    let _guard = BUNDLED_SHIM_BUILD_LOCK.lock().map_err(|_| {
        lean_rs::__host_internals::host_module_init("bundled lean-rs shim build lock is poisoned".to_owned())
    })?;
    let bootstrap =
        lean_toolchain::shared_writer::PrivateLakeBootstrap::new(Path::new(env!("CARGO_MANIFEST_DIR")), project_root)
            .map_err(|error| lean_rs::__host_internals::host_module_init(error.to_string()))?;
    let builder = bundled_capability_builder(project_root, target_name, export_signatures).private_bootstrap(bootstrap);
    builder.build_quiet().map_err(|diagnostic| {
        lean_rs::__host_internals::host_module_init(format!(
            "could not build bundled lean-rs shim target `{target_name}` under {}: {diagnostic}",
            project_root.display()
        ))
    })
}

fn bundled_capability_builder(
    project_root: &Path,
    target_name: &str,
    export_signatures: Vec<LeanExportSignature>,
) -> lean_toolchain::CargoLeanCapability {
    let mut builder = lean_toolchain::CargoLeanCapability::new(project_root, target_name)
        .package(SHIM_PACKAGE_NAME)
        .module(SHIM_LIB_NAME);
    for signature in export_signatures {
        builder = builder.export_signature(signature);
    }
    shim_sysroot(builder)
}

fn interop_capability_builder(root: &Path) -> lean_toolchain::CargoLeanCapability {
    shim_sysroot(
        lean_toolchain::CargoLeanCapability::new(root, INTEROP_LIB_NAME)
            .package("lean_rs_interop_shims")
            .module(INTEROP_LIB_NAME),
    )
}

fn shim_sysroot(builder: lean_toolchain::CargoLeanCapability) -> lean_toolchain::CargoLeanCapability {
    // Per-toolchain prep workers always receive this explicit sysroot. It must
    // participate in both build and ready keys: changing only a manifest's
    // fingerprint could otherwise relabel an artifact built by another Lean.
    match std::env::var_os("LEAN_SYSROOT") {
        Some(root) => builder.lean_sysroot(PathBuf::from(root)),
        None => builder,
    }
}

pub(crate) fn prepared_artifacts(built: &BuiltLeanCapability) -> LeanResult<super::LeanPreparedHostArtifacts> {
    use std::io::Read as _;
    let manifest_path = built.manifest_path().canonicalize().map_err(artifact_error)?;
    let mut bytes = Vec::new();
    fs::File::open(&manifest_path)
        .map_err(artifact_error)?
        .take(64 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(artifact_error)?;
    let manifest_json = String::from_utf8(bytes).map_err(|error| artifact_error(std::io::Error::other(error)))?;
    let roots = lean_toolchain::shared_writer::project_artifact_root_union(&[
        bundled_host_shims_root(),
        bundled_interop_shims_root(),
    ])
    .map_err(artifact_error)?;
    let paths = [bundled_interop_shims_root(), bundled_host_shims_root()]
        .into_iter()
        .map(|root| project_olean_search_path(&root).canonicalize().map_err(artifact_error))
        .collect::<LeanResult<Vec<_>>>()?;
    super::LeanPreparedHostArtifacts::from_snapshot(manifest_path, manifest_json, roots, paths)
}

#[cfg(unix)]
pub(crate) fn ready_artifacts(
    signatures: Vec<LeanExportSignature>,
) -> LeanResult<Option<super::LeanPreparedHostArtifacts>> {
    use std::time::{Duration, Instant};
    let host_root = bundled_host_shims_root();
    let interop_root = bundled_interop_shims_root();
    let workspaces = [host_root.clone(), interop_root.clone()];
    // No .lake creation or EX admission is allowed on this path.
    let roots = lean_toolchain::shared_writer::project_artifact_root_union(&workspaces).map_err(artifact_error)?;
    if roots.iter().any(|root| !root.join(".lake").is_dir()) {
        return Ok(None);
    }
    let helper = std::env::var_os(lean_toolchain::shared_writer::SHARED_WRITER_EXECUTABLE_ENV)
        .ok_or_else(|| artifact_error(std::io::Error::other("ready probe requires configured artifact helper")))?;
    let started = Instant::now();
    let mut ordered = roots
        .into_iter()
        .map(|root| lean_toolchain::shared_writer::project_lock_path(&root).map(|lock| (lock, root)))
        .collect::<std::io::Result<Vec<_>>>()
        .map_err(artifact_error)?;
    ordered.sort();
    let mut readers = Vec::with_capacity(ordered.len());
    for (_, root) in ordered {
        readers.push(
            lean_toolchain::shared_reader::SharedReadProxy::acquire(
                Path::new(&helper),
                &root,
                Duration::from_secs(30).saturating_sub(started.elapsed()),
            )
            .map_err(artifact_error)?,
        );
    }
    if interop_capability_builder(&interop_root)
        .probe_ready()
        .map_err(|error| artifact_error(std::io::Error::other(error)))?
        .is_none()
    {
        return Ok(None);
    }
    let builder = bundled_capability_builder(&host_root, SHIM_LIB_NAME, signatures);
    let result = builder
        .probe_ready()
        .map_err(|error| artifact_error(std::io::Error::other(error)))?
        .map(|built| prepared_artifacts(&built))
        .transpose()?;
    for reader in &mut readers {
        reader.validate().map_err(artifact_error)?;
    }
    Ok(result)
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "I/O error adapter is consumed by Result::map_err"
)]
fn artifact_error(error: std::io::Error) -> lean_rs::LeanError {
    lean_rs::__host_internals::host_module_init(error.to_string())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    struct TempProject {
        root: PathBuf,
    }

    impl TempProject {
        fn new(name: &str) -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock is after Unix epoch")
                .as_nanos();
            let root = std::env::temp_dir().join(format!("lean-rs-lake-{name}-{}-{nonce}", std::process::id()));
            fs::create_dir_all(&root).expect("create temp Lake root");
            Self { root }
        }

        fn project(&self) -> LakeProject {
            LakeProject::new(&self.root).expect("temp project opens")
        }

        fn write_manifest(&self, contents: &str) {
            fs::write(self.root.join("lake-manifest.json"), contents).expect("write manifest");
        }

        fn canonical_root(&self) -> PathBuf {
            fs::canonicalize(&self.root).expect("canonicalize temp Lake root")
        }

        fn own_olean_path(&self) -> PathBuf {
            self.canonical_root()
                .join(".lake")
                .join("build")
                .join("lib")
                .join("lean")
        }
    }

    impl Drop for TempProject {
        fn drop(&mut self) {
            drop(fs::remove_dir_all(&self.root));
        }
    }

    #[test]
    fn artifact_roots_are_owned_domains_without_creating_build_outputs() {
        let project = TempProject::new("artifact-roots");
        let roots = project.project().artifact_roots().expect("existing source roots");
        assert_eq!(roots, vec![project.canonical_root()]);
        assert!(!project.canonical_root().join(".lake").exists());
    }

    #[test]
    fn olean_search_paths_degrades_without_manifest() {
        let project = TempProject::new("no-manifest");

        assert_eq!(project.project().olean_search_paths(), vec![project.own_olean_path()]);
    }

    #[test]
    fn olean_search_paths_accepts_empty_package_manifest() {
        let project = TempProject::new("empty-manifest");
        project.write_manifest(r#"{"version":"1.2.0","packagesDir":".lake/packages","packages":[]}"#);

        assert_eq!(project.project().olean_search_paths(), vec![project.own_olean_path()]);
    }

    #[test]
    fn olean_search_paths_uses_manifest_package_entries() {
        let project = TempProject::new("packages");
        project.write_manifest(
            r#"{
                "version":"1.2.0",
                "packagesDir":"custom-packages",
                "packages":[
                    {"type":"git","name":"«doc-gen4»"},
                    {"type":"path","name":"dep","dir":"../dep"},
                    {"type":"git"},
                    "not a package"
                ]
            }"#,
        );

        assert_eq!(
            project.project().olean_search_paths(),
            vec![
                project.own_olean_path(),
                project
                    .canonical_root()
                    .join("custom-packages")
                    .join("doc-gen4")
                    .join(".lake")
                    .join("build")
                    .join("lib")
                    .join("lean"),
                project
                    .canonical_root()
                    .join("../dep")
                    .join(".lake")
                    .join("build")
                    .join("lib")
                    .join("lean"),
            ]
        );
    }

    #[test]
    fn olean_search_paths_nests_git_subdir_packages() {
        let project = TempProject::new("git-subdir");
        project.write_manifest(
            r#"{
                "version":"1.2.0",
                "packagesDir":".lake/packages",
                "packages":[
                    {"type":"git","name":"«lean-semantic-search»","subDir":"lean"},
                    {"type":"git","name":"shims","subDir":"crates/lean-rs/shims/lean-rs-interop-shims"}
                ]
            }"#,
        );

        let olean_suffix = |base: PathBuf| base.join(".lake").join("build").join("lib").join("lean");
        assert_eq!(
            project.project().olean_search_paths(),
            vec![
                project.own_olean_path(),
                olean_suffix(
                    project
                        .canonical_root()
                        .join(".lake")
                        .join("packages")
                        .join("lean-semantic-search")
                        .join("lean")
                ),
                olean_suffix(
                    project
                        .canonical_root()
                        .join(".lake")
                        .join("packages")
                        .join("shims")
                        .join("crates/lean-rs/shims/lean-rs-interop-shims")
                ),
            ]
        );
    }

    #[test]
    fn olean_search_paths_degrades_on_malformed_manifest() {
        let project = TempProject::new("malformed-manifest");
        project.write_manifest("{not json");

        assert_eq!(project.project().olean_search_paths(), vec![project.own_olean_path()]);
    }
}
