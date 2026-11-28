//! The host's view of a Lake project and of the bundled shim packages.
//!
//! [`LakeProject`] asks Lake, through [`LakeWorkspace`], for a project's
//! import roots and for the shared library of one of its `lean_lib` targets.
//! This module also builds and probes the bundled `lean-rs-host-shims`
//! package; Lake's build of its shared library builds the
//! `lean-rs-interop-shims` library it imports. Lake names every path, and
//! this module builds none.
//!
//! The type is `pub(crate)`: `LeanHost` exposes the operations callers need.

use std::cell::{OnceCell, RefCell};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use lean_rs::__host_internals::host_module_init;
use lean_rs::error::LeanResult;
use lean_toolchain::{BuiltLeanCapability, CargoLeanCapability, LakeWorkspace, LeanExportSignature};

/// Lake package name the host shim contract ships under.
pub(crate) const SHIM_PACKAGE_NAME: &str = "lean_rs_host_shims";
/// Lake `lean_lib` name inside the shim package. Constant on our side
/// because the shim package is ours; consumers don't re-declare it.
pub(crate) const SHIM_LIB_NAME: &str = "LeanRsHostShims";

const BUNDLED_SHIMS_DIR: &str = "shims";
const BUNDLED_HOST_SHIMS_DIR: &str = "lean-rs-host-shims";

static BUNDLED_SHIM_BUILD_LOCK: Mutex<()> = Mutex::new(());

/// The Lean prefix whose Lake answers for the bundled shims and the project.
///
/// This is `LEAN_SYSROOT` when set, else the prefix of the header this crate
/// was built against, so Lake uses the Lean this process links.
fn selected_sysroot() -> LeanResult<PathBuf> {
    let sysroot = if let Some(root) = std::env::var_os("LEAN_SYSROOT") {
        PathBuf::from(root)
    } else {
        Path::new(lean_toolchain::LEAN_HEADER_PATH)
            .ancestors()
            .nth(3)
            .ok_or_else(|| host_module_init("the build-selected Lean header has no sysroot"))?
            .to_owned()
    };
    fs::canonicalize(&sysroot).map_err(|error| {
        host_module_init(format!(
            "could not canonicalize the Lean sysroot {}: {error}",
            sysroot.display()
        ))
    })
}

/// The Lean prefix the bundled shims build with and the output directory it selects.
pub(crate) struct ShimOutputSelection {
    sysroot: PathBuf,
    generated_output_key: Option<&'static str>,
}

impl ShimOutputSelection {
    pub(crate) fn resolve() -> LeanResult<Self> {
        let sysroot = selected_sysroot()?;
        let generated_output_key = lean_toolchain::generated_output_key_for_sysroot(&sysroot)
            .map_err(|error| host_module_init(error.to_string()))?;
        Ok(Self {
            sysroot,
            generated_output_key,
        })
    }
}

/// A validated Lake project root and the answers Lake gave for it.
///
/// Owned by [`crate::host::LeanHost`]; never escapes the `host` module.
pub(crate) struct LakeProject {
    root: PathBuf,
    import_roots: OnceCell<Vec<PathBuf>>,
    libraries: RefCell<Vec<(String, PathBuf)>>,
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
            return Err(host_module_init(format!(
                "Lake project root '{}' does not exist or is not a directory",
                root.display()
            )));
        }
        let root = fs::canonicalize(root).map_err(|err| {
            host_module_init(format!(
                "Lake project root '{}' could not be canonicalized: {err}",
                root.display()
            ))
        })?;
        Ok(Self {
            root,
            import_roots: OnceCell::new(),
            libraries: RefCell::new(Vec::new()),
        })
    }

    /// Use `roots` as the project's import roots, so Lake never runs for them.
    pub(crate) fn set_import_roots(&mut self, roots: Vec<PathBuf>) {
        self.import_roots = OnceCell::from(roots);
    }

    /// The project's import roots in Lean's search order: dependencies first, the project last.
    ///
    /// Roots set with [`Self::set_import_roots`] come back as given.
    /// Otherwise the first call runs `lake env`, which builds nothing, and
    /// later calls reuse its answer.
    pub(crate) fn import_roots(&self) -> LeanResult<Vec<PathBuf>> {
        if let Some(roots) = self.import_roots.get() {
            return Ok(roots.clone());
        }
        let roots = self
            .workspace()?
            .lean_path()
            .map_err(|error| host_module_init(error.to_string()))?;
        Ok(self.import_roots.get_or_init(|| roots).clone())
    }

    /// The shared library Lake reports for `lib_name`, when Lake has it up to date.
    ///
    /// The first call for a name runs `lake --no-build query <lib_name>:shared`,
    /// and later calls reuse the answer, so a host loads and checks the same
    /// file. This never builds: a library Lake would rebuild is an error.
    pub(crate) fn library(&self, lib_name: &str) -> LeanResult<PathBuf> {
        if let Some((_, path)) = self.libraries.borrow().iter().find(|(name, _)| name == lib_name) {
            return Ok(path.clone());
        }
        let path = self
            .workspace()?
            .built_shared_library(lib_name)
            .map_err(|error| host_module_init(error.to_string()))?
            .ok_or_else(|| {
                host_module_init(format!(
                    "Lake would rebuild library `{lib_name}` in {}; build it before loading it",
                    self.root.display()
                ))
            })?;
        self.libraries.borrow_mut().push((lib_name.to_owned(), path.clone()));
        Ok(path)
    }

    fn workspace(&self) -> LeanResult<LakeWorkspace> {
        Ok(LakeWorkspace::new(&self.root).lean_sysroot(selected_sysroot()?))
    }

    /// Source roots passed to the source-range shim.
    ///
    /// Lean's declaration-range extension stores positions, not a stable
    /// absolute path. The Lean shim resolves a declaration's module against
    /// these roots when it needs a user-facing file label.
    pub(crate) fn source_roots(&self) -> LeanResult<Vec<PathBuf>> {
        Ok(vec![self.root.clone(), bundled_host_shims_root()])
    }

    /// Build the bundled host shims and write their manifest, which carries
    /// the trusted export signatures.
    pub(crate) fn shim_capability(
        export_signatures: Vec<LeanExportSignature>,
        selection: &ShimOutputSelection,
    ) -> LeanResult<BuiltLeanCapability> {
        let _guard = BUNDLED_SHIM_BUILD_LOCK
            .lock()
            .map_err(|_| host_module_init("bundled lean-rs shim build lock is poisoned"))?;
        build_bundled_capability(&bundled_host_shims_root(), export_signatures, selection)
    }
}

fn bundled_host_shims_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join(BUNDLED_SHIMS_DIR)
        .join(BUNDLED_HOST_SHIMS_DIR)
}

fn build_bundled_capability(
    project_root: &Path,
    export_signatures: Vec<LeanExportSignature>,
    selection: &ShimOutputSelection,
) -> LeanResult<BuiltLeanCapability> {
    bundled_capability_builder(project_root, export_signatures, selection)
        .build_quiet()
        .map_err(|diagnostic| {
            host_module_init(format!(
                "could not build the bundled lean-rs shims under {}: {diagnostic}",
                project_root.display()
            ))
        })
}

fn bundled_capability_builder(
    project_root: &Path,
    export_signatures: Vec<LeanExportSignature>,
    selection: &ShimOutputSelection,
) -> CargoLeanCapability {
    let mut builder = CargoLeanCapability::new(project_root, SHIM_LIB_NAME)
        .package(SHIM_PACKAGE_NAME)
        .module(SHIM_LIB_NAME)
        .lean_sysroot(&selection.sysroot);
    for signature in export_signatures {
        builder = builder.export_signature(signature);
    }
    if let Some(key) = selection.generated_output_key {
        builder = builder.generated_output_key(key);
    }
    builder
}

pub(crate) fn prepared_artifacts(
    built: &BuiltLeanCapability,
    selection: &ShimOutputSelection,
) -> LeanResult<super::LeanPreparedHostArtifacts> {
    prepared_artifacts_at(built, &bundled_host_shims_root(), selection)
}

/// Write the bundled shim sources into `destination` and build them there.
#[cfg(unix)]
pub(crate) fn prepare_in(
    destination: &Path,
    export_signatures: Vec<LeanExportSignature>,
    selection: &ShimOutputSelection,
) -> LeanResult<super::LeanPreparedHostArtifacts> {
    super::shim_sources::write(destination).map_err(|error| host_module_init(error.to_string()))?;
    let destination = destination
        .canonicalize()
        .map_err(|error| host_module_init(error.to_string()))?;
    let host_root = destination.join(BUNDLED_HOST_SHIMS_DIR);
    let built = build_bundled_capability(&host_root, export_signatures, selection)?;
    prepared_artifacts_at(&built, &host_root, selection)
}

/// Snapshot the manifest and take the shims' import roots from `lake env`,
/// which lists the interop package before the host package.
fn prepared_artifacts_at(
    built: &BuiltLeanCapability,
    host_root: &Path,
    selection: &ShimOutputSelection,
) -> LeanResult<super::LeanPreparedHostArtifacts> {
    use std::io::Read as _;
    let io_error = |error: std::io::Error| host_module_init(error.to_string());
    let manifest_path = built.manifest_path().canonicalize().map_err(io_error)?;
    let mut bytes = Vec::new();
    fs::File::open(&manifest_path)
        .map_err(io_error)?
        .take(64 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(io_error)?;
    let manifest_json = String::from_utf8(bytes).map_err(|error| host_module_init(error.to_string()))?;
    let paths = LakeWorkspace::new(host_root)
        .lean_sysroot(&selection.sysroot)
        .lean_path()
        .map_err(|error| host_module_init(error.to_string()))?;
    super::LeanPreparedHostArtifacts::from_snapshot(manifest_path, manifest_json, paths)
}

/// Return the bundled shims if Lake reports them up to date and their manifest matches.
///
/// This runs `lake --no-build query` and `lake env` in the bundled host-shim
/// package. Lake builds nothing, but it may write its own files there, such
/// as its configuration cache.
pub(crate) fn ready_artifacts(
    signatures: Vec<LeanExportSignature>,
    selection: &ShimOutputSelection,
) -> LeanResult<Option<super::LeanPreparedHostArtifacts>> {
    let host_root = bundled_host_shims_root();
    bundled_capability_builder(&host_root, signatures, selection)
        .probe_ready()
        .map_err(|error| host_module_init(error.to_string()))?
        .map(|built| prepared_artifacts_at(&built, &host_root, selection))
        .transpose()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::LakeProject;

    #[test]
    fn given_import_roots_come_back_without_asking_lake() -> Result<(), String> {
        // The directory has no lakefile, so asking Lake would fail.
        let root = std::env::temp_dir().join(format!("lean-rs-host-given-roots-{}", std::process::id()));
        std::fs::create_dir_all(&root).map_err(|error| error.to_string())?;
        let mut project = LakeProject::new(&root).map_err(|error| error.to_string())?;
        assert!(project.import_roots().is_err());
        let roots = vec![PathBuf::from("/deps/lib/lean"), PathBuf::from("/project/lib/lean")];
        project.set_import_roots(roots.clone());
        assert_eq!(project.import_roots().map_err(|error| error.to_string())?, roots);
        std::fs::remove_dir_all(&root).map_err(|error| error.to_string())
    }
}
