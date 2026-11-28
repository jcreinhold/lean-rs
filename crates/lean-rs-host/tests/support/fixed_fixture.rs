//! Direct host tests for a controlled SDK build. The launcher pins the actual
//! runtime, release prefix and helper before exec, then keeps them stable.
use lean_rs::{
    LeanRuntime,
    module::{LoadedHostArtifacts, TrustedHostImports},
};
use lean_rs_host::{LeanCapabilities, LeanHost, host::LeanPreparedHostArtifacts};

use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::OnceLock,
};

#[cfg(all(test, unix))]
fn retained_image(path: &Path, dev: u64, ino: u64, digest: &str) -> fs::File {
    use std::fmt::Write as _;
    use std::io::Read as _;
    use std::os::unix::fs::MetadataExt as _;
    assert_eq!(path.canonicalize().expect("canonical native image"), path);
    let named = fs::symlink_metadata(path).expect("named native image");
    assert!(named.is_file() && named.len() <= 1024 * 1024 * 1024);
    assert_eq!((named.dev(), named.ino()), (dev, ino));
    let mut file = fs::File::open(path).expect("open selected image");
    let before = file.metadata().expect("selected image metadata");
    assert!(before.is_file() && before.len() == named.len());
    assert_eq!((before.dev(), before.ino()), (dev, ino));
    let mut hash = Sha256::new();
    let mut bytes = [0; 16 * 1024];
    let mut remaining = before.len();
    loop {
        let count = file.read(&mut bytes).expect("read selected image");
        if count == 0 {
            break;
        }
        remaining = remaining.checked_sub(count as u64).expect("selected image grew");
        hash.update(bytes.get(..count).expect("read selected byte range"));
    }
    assert_eq!(remaining, 0, "selected image shrank");
    let after = file.metadata().expect("held image metadata");
    let identity = |m: &fs::Metadata| {
        (
            m.dev(),
            m.ino(),
            m.len(),
            m.mtime(),
            m.mtime_nsec(),
            m.ctime(),
            m.ctime_nsec(),
        )
    };
    assert_eq!(identity(&before), identity(&after));
    assert_eq!(
        identity(&after),
        identity(&path.metadata().expect("recheck named image"))
    );
    let mut actual = String::with_capacity(64);
    for byte in hash.finalize() {
        write!(actual, "{byte:02x}").expect("write image digest");
    }
    assert_eq!(actual, digest);
    file
}

#[cfg(all(test, unix))]
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OrdinaryShims {
    prefix: PathBuf,
    manifest: PathBuf,
    manifest_sha256: String,
    search_paths: Vec<PathBuf>,
    images: [(PathBuf, u64, u64, String); 2],
    package_identity: Option<(u64, u64)>,
}

#[cfg(all(test, unix))]
struct OrdinaryFixture {
    project: PathBuf,
    project_image: PathBuf,
    shims: Option<(OrdinaryShims, String)>,
    // The job owns its private project and the selected shims. These files
    // keep the verified images open until exit.
    _files: Vec<fs::File>,
}

#[cfg(all(test, unix))]
fn ordinary_fixture() -> Option<&'static OrdinaryFixture> {
    use std::io::Read as _;
    use std::os::unix::fs::MetadataExt as _;
    let marker = "LEAN_RS_TEST_SCALAR_PRIVATE_JOB";
    let compiled = option_env!("LEAN_RS_TEST_SCALAR_PRIVATE_JOB");
    let runtime = std::env::var(marker).ok();
    if compiled.is_none() && std::env::var_os(marker).is_none() {
        return None;
    }
    assert_eq!(compiled, Some("1"));
    assert_eq!(runtime.as_deref(), Some("1"));
    static INSTANCE: OnceLock<OrdinaryFixture> = OnceLock::new();
    Some(INSTANCE.get_or_init(|| {
        let header = Path::new(lean_toolchain::LEAN_HEADER_PATH);
        assert_eq!(
            std::env::var_os("LEAN_SYSROOT").as_deref(),
            header.ancestors().nth(3).map(Path::as_os_str)
        );
        assert!(std::env::var_os("LEAN_PATH").is_none());
        let checkout = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .expect("canonical SDK checkout");
        let (project, dev, ino): (PathBuf, u64, u64) =
            serde_json::from_str(option_env!("LEAN_RS_TEST_SCALAR_FIXTURE").unwrap_or_default())
                .expect("ordinary test requires the selected scalar fixture");
        assert_eq!(project, checkout.join("fixtures/lean"));
        assert_eq!(project.canonicalize().expect("canonical fixture"), project);
        let metadata = project.metadata().expect("selected fixture metadata");
        assert_eq!(metadata.mode() & 0o7777, 0o700);
        assert_eq!(metadata.uid(), checkout.metadata().expect("checkout metadata").uid());
        let extension = std::env::consts::DLL_EXTENSION;
        let project_image = project.join(format!(
            ".lake/build/lib/liblean__rs__fixture_LeanRsFixture.{extension}"
        ));
        let mut files = vec![retained_image(
            &project_image,
            dev,
            ino,
            option_env!("LEAN_RS_PROFILE_FIXTURE_SHA256").unwrap_or_default(),
        )];

        let shims = option_env!("LEAN_RS_TEST_ORDINARY_HOST_SHIMS").map(|selection| {
            let shims: OrdinaryShims = serde_json::from_str(selection).expect("prepared host shim selection");
            assert_eq!(
                std::env::var_os("LEAN_SYSROOT").as_deref(),
                Some(shims.prefix.as_os_str())
            );
            let package = if let Some(identity) = shims.package_identity {
                let package = checkout.join("target/metadata-qualification/shim-package");
                assert_eq!(
                    package.canonicalize().expect("canonical workspace shim package"),
                    package
                );
                let metadata = package.metadata().expect("workspace shim package");
                assert_eq!((metadata.dev(), metadata.ino()), identity);
                assert!(metadata.is_dir());
                assert_eq!(metadata.uid(), checkout.metadata().expect("checkout owner").uid());
                assert_eq!(metadata.mode() & 0o7777, 0o700);
                package
            } else {
                checkout.clone()
            };
            let workspace = package.join("crates/lean-rs-host/shims");
            let host = workspace.join("lean-rs-host-shims");
            let interop = workspace.join("lean-rs-interop-shims");
            let generated = ".lake/lean-rs-generated/lean-4.36.0";
            let host_build = host.join(generated);
            let interop_build = interop.join(generated);
            assert_eq!(
                shims.search_paths,
                [interop_build.join("lib/lean"), host_build.join("lib/lean")]
            );
            // The manifest sits beside the library Lake built.
            assert_eq!(shims.manifest.parent(), Some(host_build.join("lib").as_path()));
            for (pin, path) in shims.images.iter().zip([
                host_build.join(format!("lib/liblean__rs__host__shims_LeanRsHostShims.{extension}")),
                interop_build.join(format!("lib/liblean__rs__interop__shims_LeanRsInterop.{extension}")),
            ]) {
                assert_eq!(pin.0, path);
            }

            for (image, dev, ino, hash) in &shims.images {
                files.push(retained_image(image, *dev, *ino, hash));
            }
            assert_eq!(
                shims.manifest.canonicalize().expect("canonical shim manifest"),
                shims.manifest
            );
            let mut bytes = Vec::new();
            fs::File::open(&shims.manifest)
                .expect("open prepared shim manifest")
                .take(64 * 1024 + 1)
                .read_to_end(&mut bytes)
                .expect("bounded shim manifest");
            assert!(bytes.len() <= 64 * 1024);
            assert_eq!(hex_digest(&bytes), shims.manifest_sha256);
            // The named SDK run already checked the exact manifest’s native
            // dependencies and initializer order. The verified bytes are the
            // same bytes passed to the loader.
            (shims, String::from_utf8(bytes).expect("SDK manifest text"))
        });
        OrdinaryFixture {
            project,
            project_image,
            shims,
            _files: files,
        }
    }))
}

#[allow(dead_code, reason = "shared with separate strict test binaries")]
pub(super) fn native_fixture_selected() -> bool {
    #[cfg(all(test, unix))]
    {
        ordinary_fixture().is_some()
    }
    #[cfg(not(all(test, unix)))]
    {
        false
    }
}

#[allow(dead_code, reason = "shared with separate strict test binaries")]
pub(super) fn native_fixture_root() -> PathBuf {
    #[cfg(all(test, unix))]
    if let Some(fixture) = ordinary_fixture() {
        return fixture.project.clone();
    }
    fixture_root()
}

#[allow(dead_code, reason = "used by the Lean import probes")]
pub(super) fn native_shim_search_paths() -> Vec<PathBuf> {
    #[cfg(all(test, unix))]
    if let Some(fixture) = ordinary_fixture() {
        let (shims, _) = fixture.shims.as_ref().expect("selected host shims");
        return shims.search_paths.clone();
    }
    lean_rs_host::host::prepare_host_artifacts()
        .expect("prepare bundled shims")
        .shim_search_paths()
        .to_vec()
}

#[allow(dead_code, reason = "shared with separate strict test binaries")]
pub(super) fn native_project_library() -> lean_rs::module::LeanLibrary<'static> {
    #[cfg(all(test, unix))]
    if let Some(fixture) = ordinary_fixture() {
        #[allow(
            unsafe_code,
            reason = "the private test job owns the exact source-built fixture through exit"
        )]
        // SAFETY: The trusted job built and selected this exact library and
        // its runtime. All native images and retained input custody live to
        // process exit, including after the returned library is dropped.
        let load = || {
            unsafe {
                lean_rs::module::LeanLibrary::open(LeanRuntime::init().expect("paired runtime"), &fixture.project_image)
            }
            .expect("load selected project image")
        };
        static RETAINED: OnceLock<()> = OnceLock::new();
        RETAINED.get_or_init(|| {
            Box::leak(Box::new(load()));
        });
        return load();
    }
    project_library()
}

#[allow(dead_code, reason = "shared with separate strict test binaries")]
pub(super) fn native_capabilities<'h>(
    host: &'h LeanHost<'static>,
) -> lean_rs::LeanResult<LeanCapabilities<'static, 'h>> {
    native_capabilities_for(host, true)
}

#[allow(dead_code, reason = "shared with separate strict test binaries")]
pub(super) fn native_shims_only<'h>(host: &'h LeanHost<'static>) -> lean_rs::LeanResult<LeanCapabilities<'static, 'h>> {
    native_capabilities_for(host, false)
}

fn native_capabilities_for<'h>(
    host: &'h LeanHost<'static>,
    load_project: bool,
) -> lean_rs::LeanResult<LeanCapabilities<'static, 'h>> {
    #[cfg(all(test, unix))]
    if let Some(fixture) = ordinary_fixture() {
        let (shims, text) = fixture
            .shims
            .as_ref()
            .ok_or_else(|| lean_rs::error::host_module_init("run the existing broken-import step to select shims"))?;
        let prepared =
            LeanPreparedHostArtifacts::from_snapshot(shims.manifest.clone(), text.clone(), shims.search_paths.clone())?;
        let runtime = LeanRuntime::init()?;
        #[allow(
            unsafe_code,
            reason = "the exact SDK shim ABI and images are verified and the job keeps them unchanged"
        )]
        // SAFETY: The trusted job verified the SDK build and runtime before
        // launch and rebuilds nothing until exit. We checked the actual
        // manifest and both retained shim images.
        let loaded = unsafe {
            LoadedHostArtifacts::load(
                runtime,
                shims.manifest.clone(),
                text.as_bytes().to_vec(),
                shims.search_paths.clone(),
                fixture,
            )?
        };
        if !load_project {
            return LeanCapabilities::from_prepared(host, &prepared, loaded);
        }
        return LeanCapabilities::from_prepared_capability(
            host,
            &prepared,
            loaded,
            (
                native_project_library(),
                lean_rs::module::LeanModuleInitializer::new("lean_rs_fixture", "LeanRsFixture"),
            ),
            "lean_rs_fixture",
            "LeanRsFixture",
        );
    }
    if load_project {
        capabilities(host)
    } else {
        shims_only(host)
    }
}

#[derive(Deserialize)]
struct Record {
    scratch: PathBuf,
    prefix: PathBuf,
    helper: PathBuf,
    helper_sha256: String,
    custody_roots: Vec<PathBuf>,
    artifact_sha256: BTreeMap<PathBuf, String>,
}
// Created after linking by the trusted fixture owner in the held inspection
// directory. The launcher verifies the identical executable and actual loader
// closure before exec; application code cannot retroactively constrain dyld.
#[derive(Deserialize)]
struct LinkedReview {
    executable: PathBuf,
    executable_sha256: String,
    source_sha256: String,
    release_inventory_sha256: String,
    graph_policy_sha256: String,
}

fn text_digest(value: &str) -> String {
    hex_digest(value.as_bytes())
}

fn hex_digest(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut output = String::with_capacity(64);
    for byte in Sha256::digest(bytes) {
        write!(output, "{byte:02x}").expect("write to String");
    }
    output
}

struct Admission {
    record: Record,
    approved_rows: Vec<serde_json::Value>,
}
fn digest(path: &Path) -> String {
    hex_digest(&fs::read(path).expect("read controlled fixture input"))
}
fn reviewed_input<'a>(value: Option<&'a str>, required: &str) -> &'a str {
    value.expect(required)
}

fn admission() -> &'static Admission {
    static INSTANCE: OnceLock<Admission> = OnceLock::new();
    INSTANCE.get_or_init(|| {
        let reviewed_record = reviewed_input(
            option_env!("LEAN_RS_TEST_REVIEWED_RECORD"),
            "qualification build requires a protected fixture record",
        );
        let record: Record = serde_json::from_str(reviewed_record).expect("valid fixture record");
        let header = Path::new(lean_toolchain::LEAN_HEADER_PATH);
        assert_eq!(Some(record.prefix.as_path()), header.ancestors().nth(3));
        assert_eq!(
            std::env::var_os("LEAN_SYSROOT").as_deref(),
            Some(record.prefix.as_os_str())
        );
        assert!(std::env::var_os("LEAN_PATH").is_none());
        assert_eq!(record.helper.canonicalize().expect("canonical helper"), record.helper);
        assert_eq!(digest(&record.helper), record.helper_sha256);
        let roots = [
            "lean-rs-host-shims",
            "lean-rs-interop-shims",
            "fixture",
            "bin",
            "inspection",
        ]
        .map(|r| record.scratch.join(r));
        assert_eq!(record.custody_roots, roots);
        // The qualification launcher owns these private inputs exclusively for the complete run.
        // Compile-time selection is deliberate: a normal test process must not infer that
        // guarantee from the runtime environment or from matching file hashes.
        assert_eq!(
            option_env!("LEAN_RS_TEST_EXCLUSIVE_FIXTURE_RECORD_SHA256"),
            Some(text_digest(reviewed_record).as_str()),
            "launcher must independently select the exclusively owned fixture record"
        );
        let linked = roots[4].join("host-services-linked-review.json");
        assert_eq!(linked.canonicalize().expect("post-link review"), linked);
        let review: LinkedReview =
            serde_json::from_slice(&fs::read(linked).expect("held post-link record")).expect("review record");
        let executable = std::env::current_exe()
            .expect("running binary")
            .canonicalize()
            .expect("canonical binary");
        assert!(executable.starts_with(&roots[3]));
        assert_eq!(executable, review.executable);
        assert_eq!(digest(&executable), review.executable_sha256);
        let source = reviewed_input(
            option_env!("LEAN_RS_TEST_SOURCE_SHA256"),
            "reviewed source closure identity",
        );
        assert!(source.len() == 64 && source.bytes().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(source, review.source_sha256);
        let inventory_path = roots[4].join("release-inventory.json");
        assert_eq!(
            inventory_path.canonicalize().expect("held release inventory"),
            inventory_path
        );
        let expected = reviewed_input(
            option_env!("LEAN_RS_TEST_RELEASE_INVENTORY_SHA256"),
            "authenticated transitive release inventory digest",
        );
        assert_eq!(digest(&inventory_path), expected);
        assert_eq!(review.release_inventory_sha256, expected);
        let release: BTreeMap<PathBuf, String> =
            serde_json::from_slice(&fs::read(&inventory_path).expect("release inventory"))
                .expect("release file digests");
        assert!(release.contains_key(Path::new("bin/lean")) && release.contains_key(Path::new("include/lean/lean.h")));
        for (relative, expected) in release {
            assert!(
                relative
                    .components()
                    .all(|component| matches!(component, std::path::Component::Normal(_)))
            );
            let path = record.prefix.join(relative);
            assert_eq!(path.canonicalize().expect("release input"), path);
            assert_eq!(digest(&path), expected, "release closure input {}", path.display());
        }
        let graph_policy = reviewed_input(
            option_env!("LEAN_RS_TEST_APPROVED_GRAPHS"),
            "independent graph and request policy",
        );
        assert_eq!(text_digest(graph_policy), review.graph_policy_sha256);
        let approved_rows = serde_json::from_str(graph_policy).expect("valid approved rows");
        for (p, h) in &record.artifact_sha256 {
            assert_eq!(p.canonicalize().expect("canonical fixture input"), *p);
            assert!(p.starts_with(&record.prefix) || roots.iter().any(|root| p.starts_with(root)));
            assert_eq!(digest(p), *h, "input changed: {}", p.display());
        }
        // Include every file in each ordered import directory (including sidecars).
        // Unlisted new oleans could otherwise shadow an admitted transitive import.
        let mut inventory: BTreeMap<PathBuf, String> = serde_json::from_str(reviewed_input(
            option_env!("LEAN_RS_TEST_REVIEWED_IMPORT_FILES"),
            "qualification build requires a complete reviewed import inventory",
        ))
        .expect("valid import inventory");
        let mut pending = roots[..3]
            .iter()
            .map(|r| r.join(".lake/build/lib/lean"))
            .collect::<Vec<_>>();
        // Independently built user-project snapshots live under the existing fixture
        // loan. Inventory every file here, including package sources and dependencies.
        let projects = roots[2].join("host-service-projects");
        if projects.exists() {
            pending.push(projects);
        }
        while let Some(p) = pending.pop() {
            assert_eq!(p.canonicalize().expect("existing import input"), p);
            if p.is_dir() {
                for entry in fs::read_dir(&p).expect("import directory") {
                    pending.push(entry.expect("import entry").path());
                }
            } else {
                assert!(p.is_file());
                assert_eq!(
                    inventory
                        .remove(p.strip_prefix(&record.scratch).expect("private input"))
                        .as_deref(),
                    Some(digest(&p).as_str())
                );
            }
        }
        assert!(inventory.is_empty());
        Admission { record, approved_rows }
    })
}
pub(super) fn fixture_root() -> PathBuf {
    admission().record.scratch.join("fixture")
}
fn paths() -> Vec<PathBuf> {
    let root = &admission().record.scratch;
    ["fixture", "lean-rs-interop-shims", "lean-rs-host-shims"]
        .map(|n| root.join(n).join(".lake/build/lib/lean"))
        .to_vec()
}
pub(super) fn capabilities<'h>(host: &'h LeanHost<'static>) -> lean_rs::LeanResult<LeanCapabilities<'static, 'h>> {
    open(host, true)
}
#[allow(
    dead_code,
    reason = "this fixture entry is shared by separate test and profiling binaries"
)]
pub(super) fn shims_only<'h>(host: &'h LeanHost<'static>) -> lean_rs::LeanResult<LeanCapabilities<'static, 'h>> {
    open(host, false)
}
fn open<'h>(host: &'h LeanHost<'static>, load_project: bool) -> lean_rs::LeanResult<LeanCapabilities<'static, 'h>> {
    let record = &admission().record;
    let host_root = record.scratch.join("lean-rs-host-shims");
    let manifest = host_root.join(".lake/qualification-manifest.json");
    assert!(record.artifact_sha256.contains_key(&manifest));
    let manifest_json =
        fs::read_to_string(&manifest).map_err(|err| lean_rs::error::host_module_init(err.to_string()))?;
    let shim_paths = paths().into_iter().skip(1).collect::<Vec<_>>();
    let prepared =
        LeanPreparedHostArtifacts::from_snapshot(manifest.clone(), manifest_json.clone(), shim_paths.clone())?;
    let rt = LeanRuntime::init()?;
    #[allow(
        unsafe_code,
        reason = "fixed qualified snapshot; the launching harness pins the actual runtime and artifact closure"
    )]
    // SAFETY: admission verified the reviewed image and manifest bytes under the fixture
    // loans. The protected record approves these exact init symbols and visibility.
    // Keep a permanent image reference for Lean's process-global registrations.
    let shim = unsafe { LoadedHostArtifacts::load(rt, manifest, manifest_json.into_bytes(), shim_paths, ())? };
    if !load_project {
        return LeanCapabilities::from_prepared(host, &prepared, shim);
    }
    let root_manifest = fixture_root().join(".lake/qualification-manifest.json");
    let project = host
        .project_library_path("LeanRsFixture")?
        .canonicalize()
        .map_err(|err| lean_rs::error::host_module_init(err.to_string()))?;
    assert!(record.artifact_sha256.contains_key(&root_manifest) && record.artifact_sha256.contains_key(&project));
    let project_desc: serde_json::Value = serde_json::from_slice(
        &fs::read(root_manifest).map_err(|err| lean_rs::error::host_module_init(err.to_string()))?,
    )
    .map_err(|err| lean_rs::error::host_module_init(err.to_string()))?;
    assert_eq!(
        project_desc.get("primary_dylib").and_then(serde_json::Value::as_str),
        project.to_str()
    );
    let library = project_library();
    LeanCapabilities::from_prepared_capability(
        host,
        &prepared,
        shim,
        (
            library,
            lean_rs::module::LeanModuleInitializer::new("lean_rs_fixture", "LeanRsFixture"),
        ),
        "lean_rs_fixture",
        "LeanRsFixture",
    )
}
#[allow(
    dead_code,
    reason = "this fixture entry is shared by separate test and profiling binaries"
)]
pub(super) fn imports<'lean, 'c>(caps: &'c LeanCapabilities<'lean, '_>, module: &str) -> TrustedHostImports<'lean, 'c> {
    imports_for(caps, &[module], lean_rs_host::LeanSessionImportProfile::Private)
}
#[allow(
    dead_code,
    reason = "this fixture entry is shared by separate test and profiling binaries"
)]
pub(super) fn imports_for<'lean, 'c>(
    caps: &'c LeanCapabilities<'lean, '_>,
    names: &[&str],
    profile: lean_rs_host::LeanSessionImportProfile,
) -> TrustedHostImports<'lean, 'c> {
    imports_for_fixture(caps, names, profile, false)
}

#[allow(dead_code, reason = "shared with separate strict test binaries")]
pub(super) fn native_imports<'lean, 'c>(
    caps: &'c LeanCapabilities<'lean, '_>,
    module: &str,
) -> TrustedHostImports<'lean, 'c> {
    native_imports_for(caps, &[module], lean_rs_host::LeanSessionImportProfile::Private)
}

#[allow(dead_code, reason = "shared with separate strict test binaries")]
pub(super) fn native_imports_for<'lean, 'c>(
    caps: &'c LeanCapabilities<'lean, '_>,
    names: &[&str],
    profile: lean_rs_host::LeanSessionImportProfile,
) -> TrustedHostImports<'lean, 'c> {
    imports_for_fixture(caps, names, profile, true)
}

fn imports_for_fixture<'lean, 'c>(
    caps: &'c LeanCapabilities<'lean, '_>,
    names: &[&str],
    profile: lean_rs_host::LeanSessionImportProfile,
    ordinary: bool,
) -> TrustedHostImports<'lean, 'c> {
    assert!(matches!(
        names,
        ["LeanRsFixture.Handles"
            | "LeanRsFixture.SourceRanges"
            | "LeanRsHostShims.Elaboration"
            | "LeanRsHostShims.Meta"
            | "LeanRsHostShims.InfoTree"
            | "LeanRsFixture.Meta"
            | "LeanRsFixtureLateExtension"]
            | ["LeanRsFixture.Handles" | "LeanRsFixture.Meta", "LeanRsHostShims.Meta"]
    ));
    assert!(
        profile == lean_rs_host::LeanSessionImportProfile::Private
            || (names == ["LeanRsFixture.Handles"]
                && profile == lean_rs_host::LeanSessionImportProfile::FullPrivateCompat)
    );
    let options = lean_rs_host::LeanImportProfileMode::FullSession(profile)
        .native_options(&lean_rs_host::LeanImportProfilerOptions::default());
    #[cfg(all(test, unix))]
    let ordinary_paths = if ordinary {
        native_import_paths(caps, names)
    } else {
        None
    };
    let strict_paths = || {
        for name in names {
            let relative = PathBuf::from(name.replace('.', "/")).with_extension("olean");
            assert!(
                paths().iter().any(|root| root.join(&relative).is_file()),
                "missing admitted direct module {name}"
            );
        }
        approved_graph(caps, names, &options, "session")
    };
    #[cfg(all(test, unix))]
    let roots = ordinary_paths.unwrap_or_else(strict_paths);
    #[cfg(not(all(test, unix)))]
    let roots = {
        let _ = ordinary;
        strict_paths()
    };
    #[allow(
        unsafe_code,
        reason = "two fixed source-reviewed imports in the admitted ordered fixture and release closure"
    )]
    // SAFETY: The same finite graph and option policy applies to both
    // profiles. The strict route checks its reviewed inventory; the ordinary
    // test job owns the source-built fixture and holds the exact shim roots.
    unsafe {
        caps.shim_capability().import_scope_with_options(
            roots,
            names.iter().map(|n| (*n).to_owned()).collect(),
            options,
        )
    }
}

#[allow(dead_code, reason = "host services only")]
pub(super) fn native_bracketed_handles_paths(caps: &LeanCapabilities<'_, '_>) -> Option<Vec<String>> {
    #[cfg(all(test, unix))]
    {
        native_import_paths(caps, &["LeanRsFixture.Handles"])
    }
    #[cfg(not(all(test, unix)))]
    {
        let _ = caps;
        None
    }
}

#[cfg(all(test, unix))]
fn native_import_paths(caps: &LeanCapabilities<'_, '_>, names: &[&str]) -> Option<Vec<String>> {
    ordinary_fixture().map(|fixture| checked_native_import_paths(fixture, caps, names))
}

#[cfg(all(test, unix))]
fn checked_native_import_paths(
    fixture: &OrdinaryFixture,
    caps: &LeanCapabilities<'_, '_>,
    names: &[&str],
) -> Vec<String> {
    let (shims, _) = fixture.shims.as_ref().expect("same prepared host shims");
    let paths = std::iter::once(fixture.project.join(".lake/build/lib/lean"))
        .chain(shims.search_paths.iter().cloned())
        .collect::<Vec<_>>();
    let actual = caps.import_search_paths();
    assert!(actual.iter().map(Path::new).eq(paths.iter().map(PathBuf::as_path)));
    let [(host_image, ..), _] = &shims.images;
    assert_eq!(caps.shim_capability().library_path(), host_image);
    for root in &paths {
        assert_eq!(root.canonicalize().expect("canonical test import root"), *root);
        for prefix in ["Init", "Lean", "Lake", "Std"] {
            assert!(
                !root.join(prefix).exists() && !root.join(format!("{prefix}.olean")).exists(),
                "selected fixture shadows installed module {prefix}"
            );
        }
    }
    for name in names {
        let relative = PathBuf::from(name.replace('.', "/")).with_extension("olean");
        let mut named = paths
            .iter()
            .map(|root| root.join(&relative))
            .filter(|path| path.exists());
        let path = named.next().expect("selected direct module exists");
        assert!(named.next().is_none(), "ambiguous direct module {name}");
        assert!(path.is_file());
        assert_eq!(path.canonicalize().expect("canonical selected module"), path);
    }
    actual
}

/// A separately reviewed build-time policy approves exact graph requests for these
/// binaries. Hashing the fixture inventory alone cannot authorize loading extensions.
pub(super) fn approved_graph(
    caps: &LeanCapabilities<'_, '_>,
    names: &[&str],
    options: &lean_rs::module::HostImportOptions,
    operation: &str,
) -> Vec<String> {
    let roots = caps.import_search_paths();
    let admitted = paths();
    let project_custody = fixture_root().join("host-service-projects");
    for root in &roots {
        let root = PathBuf::from(root);
        assert_eq!(root.canonicalize().expect("import directory in custody"), root);
        assert!(admitted.contains(&root) || root.starts_with(&project_custody));
    }
    for name in names {
        assert!(
            name.split('.')
                .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'))
        );
        let relative = PathBuf::from(name.replace('.', "/")).with_extension("olean");
        let release = PathBuf::from(release_import_path());
        let found =
            roots.iter().any(|root| Path::new(root).join(&relative).is_file()) || release.join(&relative).is_file();
        if matches!(*name, "Broken" | "Dep.NonExistent") {
            assert!(!found, "negative import must be absent under every admitted root");
        } else {
            assert!(found, "missing admitted module {name}");
        }
    }
    assert!(
        options.trace_output.is_empty(),
        "file trace output requires separate destination custody"
    );
    let binding = |value: Option<&str>| text_digest(value.expect("protected fixture input"));
    let row = serde_json::json!({
        "record": binding(option_env!("LEAN_RS_TEST_REVIEWED_RECORD")),
        "inventory": binding(option_env!("LEAN_RS_TEST_REVIEWED_IMPORT_FILES")),
        "operation": operation, "roots": roots, "imports": names,
        "import_all": options.import_all, "level": options.level, "load_extensions": options.load_extensions,
        "profiler": options.profiler, "trace_profiler": options.trace_profiler, "trace_output": options.trace_output,
    });
    assert!(
        admission().approved_rows.contains(&row),
        "unapproved fixture import graph"
    );
    roots
}

pub(super) fn release_import_path() -> String {
    admission()
        .record
        .prefix
        .join("lib/lean")
        .to_str()
        .expect("release search path")
        .into()
}

#[allow(
    dead_code,
    reason = "this fixture entry is shared by separate test and profiling binaries"
)]
pub(super) fn resolution() -> Vec<String> {
    let mut p = paths();
    p.push(admission().record.prefix.join("lib/lean"));
    p.into_iter()
        .map(|x| x.to_str().expect("UTF-8 fixture path").to_owned())
        .collect()
}
#[allow(
    dead_code,
    reason = "this fixture entry is shared by separate test and profiling binaries"
)]
pub(super) fn term<'lean, 'c>(
    caps: &'c LeanCapabilities<'lean, '_>,
    session: &lean_rs_host::LeanSession<'lean, '_>,
    source: &str,
) -> lean_rs::host_bridge::TrustedHostTerm<'lean, 'c> {
    term_with_paths(caps, session, source, || {
        if source == "(1 + 1 : Nat)" && native_fixture_selected() {
            native_elaboration_paths(caps, session)
        } else {
            resolution()
        }
    })
}

#[allow(dead_code, reason = "used only by diagnostics tests")]
pub(super) fn native_parse_error_term<'lean, 'c>(
    caps: &'c LeanCapabilities<'lean, '_>,
    session: &lean_rs_host::LeanSession<'lean, '_>,
) -> lean_rs::host_bridge::TrustedHostTerm<'lean, 'c> {
    term_with_paths(caps, session, "(1 +", || {
        if native_fixture_selected() {
            native_elaboration_paths(caps, session)
        } else {
            resolution()
        }
    })
}

#[allow(clippy::panic, reason = "reject every unlisted source expression")]
fn term_with_paths<'lean, 'c>(
    caps: &'c LeanCapabilities<'lean, '_>,
    session: &lean_rs_host::LeanSession<'lean, '_>,
    source: &str,
    paths: impl FnOnce() -> Vec<String>,
) -> lean_rs::host_bridge::TrustedHostTerm<'lean, 'c> {
    let modules = match source {
        "(1 + 1 : Nat)" | "(1 +" => vec!["LeanRsHostShims.Elaboration".into()],
        "(1 + 2 : Nat)" | "(1 * 7 : Nat)" => vec!["LeanRsFixture.Handles".into(), "LeanRsHostShims.Meta".into()],
        _ => panic!("unapproved term"),
    };
    let (environment, imports) = session.source_context();
    assert!(imports.matches_request(&caps.import_search_paths(), &modules));
    assert_eq!(imports.options(), &lean_rs::module::HostImportOptions::default());
    #[allow(unsafe_code, reason = "closed well-typed and parse-error test cases")]
    // SAFETY: these exact test expressions use the pinned default prelude elaborators,
    // no expected type, the admitted fixture graph and the fixed default source options.
    unsafe {
        caps.shim_capability().term_scope(
            environment,
            imports,
            paths(),
            source.to_owned(),
            None,
            lean_rs_host::LeanElabOptions::new().source_options(),
        )
    }
    .expect("attest fixture term")
}
#[allow(dead_code, reason = "host services only")]
pub(super) fn native_meta_term<'lean, 'c>(
    caps: &'c LeanCapabilities<'lean, '_>,
    session: &lean_rs_host::LeanSession<'lean, '_>,
    source: &str,
) -> lean_rs::host_bridge::TrustedHostTerm<'lean, 'c> {
    const MODULES: &[&str] = &["LeanRsFixture.Meta", "LeanRsHostShims.Meta"];
    assert!(matches!(
        source,
        "Nat"
            | "Bool"
            | "LeanRsFixture.Meta.ReducibleNatAlias"
            | "LeanRsFixture.Meta.IrreducibleNatAlias"
            | "(Nat.succ 0 : Nat)"
    ));
    let (env, imports) = session.source_context();
    assert!(imports.matches_request(
        &caps.import_search_paths(),
        &MODULES.iter().map(|name| (*name).to_owned()).collect::<Vec<_>>()
    ));
    assert_eq!(imports.options(), &lean_rs::module::HostImportOptions::default());
    let paths = native_source_paths(caps, MODULES).expect("ordinary Meta fixture selected");
    #[allow(unsafe_code, reason = "five fixed terms in the held test fixture")]
    // SAFETY: the fixed terms use the exact loaded Meta environment, native
    // roots under held readers, and the default elaborator with no expected type.
    unsafe {
        caps.shim_capability().term_scope(
            env,
            imports,
            paths,
            source.to_owned(),
            None,
            lean_rs_host::LeanElabOptions::new().source_options(),
        )
    }
    .expect("attest fixed Meta term")
}

#[allow(dead_code, reason = "host services only")]
#[derive(Clone, Copy)]
pub(super) enum HeldHostImport {
    HeaderAccounting,
    FullProfiles(lean_rs_host::LeanSessionImportProfile),
}

impl HeldHostImport {
    fn modules(self) -> &'static [&'static str] {
        match self {
            Self::HeaderAccounting => &["Lean"],
            Self::FullProfiles(_) => &["LeanRsFixture.Handles", "LeanRsHostShims.Elaboration"],
        }
    }

    fn options(self) -> lean_rs::module::HostImportOptions {
        let profile = match self {
            Self::HeaderAccounting => lean_rs_host::LeanSessionImportProfile::Private,
            Self::FullProfiles(profile) => profile,
        };
        lean_rs_host::LeanImportProfileMode::FullSession(profile)
            .native_options(&lean_rs_host::LeanImportProfilerOptions::default())
    }

    fn roots(self, caps: &LeanCapabilities<'_, '_>) -> (Vec<String>, PathBuf) {
        // The header case must find Lean in the installed release, never in
        // the explicit fixture paths. The existing path check forbids shadows.
        let local = match self {
            Self::HeaderAccounting => &[][..],
            Self::FullProfiles(_) => self.modules(),
        };
        #[cfg(all(test, unix))]
        let explicit = native_import_paths(caps, local);
        #[cfg(not(all(test, unix)))]
        let explicit: Option<Vec<String>> = {
            let _ = (caps, local);
            None
        };
        let paths = explicit.expect("ordinary host fixture selected");
        let prefix = Path::new(lean_toolchain::LEAN_HEADER_PATH)
            .ancestors()
            .nth(3)
            .expect("build-selected Lean prefix");
        assert_eq!(std::env::var_os("LEAN_SYSROOT").as_deref(), Some(prefix.as_os_str()));
        assert!(std::env::var_os("LEAN_PATH").is_none());
        let release = prefix.join("lib/lean");
        assert_eq!(release.canonicalize().expect("canonical Lean library"), release);
        let lean = release.join("Lean.olean");
        assert!(lean.is_file() && lean.canonicalize().expect("canonical Lean module") == lean);
        (paths, release)
    }

    #[allow(dead_code, reason = "host services only")]
    pub(super) fn imports<'lean, 'c>(self, caps: &'c LeanCapabilities<'lean, '_>) -> TrustedHostImports<'lean, 'c> {
        let (paths, _) = self.roots(caps);
        let names = self.modules().iter().map(|name| (*name).into()).collect();
        #[allow(unsafe_code, reason = "the held fixture covers only these two fixed import lists")]
        // SAFETY: the verified user and shim roots and installed release cover
        // the fixed imports. Profile options come from the supported full-session
        // modes with profiling and file output disabled.
        unsafe {
            caps.shim_capability()
                .import_scope_with_options(paths, names, self.options())
        }
    }

    #[allow(dead_code, reason = "host services only")]
    pub(super) fn source_paths(
        self,
        caps: &LeanCapabilities<'_, '_>,
        session: &lean_rs_host::LeanSession<'_, '_>,
    ) -> Vec<String> {
        assert!(!matches!(
            self,
            Self::FullProfiles(
                lean_rs_host::LeanSessionImportProfile::ExportedPublic | lean_rs_host::LeanSessionImportProfile::Server
            )
        ));
        let (mut paths, release) = self.roots(caps);
        let (_, imports) = session.source_context();
        imports
            .check_capability(caps.shim_capability())
            .expect("same held capability");
        let names = self.modules().iter().map(|name| (*name).into()).collect::<Vec<_>>();
        assert!(imports.matches_request(&paths, &names));
        assert_eq!(imports.options(), &self.options());
        paths.push(release.to_str().expect("UTF-8 Lean path").to_owned());
        paths
    }
}

#[allow(dead_code, reason = "host services only")]
pub(super) fn native_elaboration_paths(
    caps: &LeanCapabilities<'_, '_>,
    session: &lean_rs_host::LeanSession<'_, '_>,
) -> Vec<String> {
    const MODULE: &str = "LeanRsHostShims.Elaboration";
    let (_, imports) = session.source_context();
    assert!(imports.matches_request(&caps.import_search_paths(), &[MODULE.into()]));
    assert_eq!(imports.options(), &lean_rs::module::HostImportOptions::default());
    native_source_paths(caps, &[MODULE]).expect("ordinary elaboration fixture selected")
}

#[allow(dead_code, reason = "host services only")]
pub(super) fn native_module_syntax_imports<'lean, 'c>(
    caps: &'c LeanCapabilities<'lean, '_>,
    names: &[&str],
) -> TrustedHostImports<'lean, 'c> {
    let roots = native_module_syntax_paths(caps, names, false);
    #[allow(unsafe_code, reason = "two exact import orders in the held module syntax fixture")]
    // SAFETY: the ordinary fixture owns the fixed source-built modules and holds the
    // selected shim inputs. No additional roots, imports or options are admitted.
    unsafe {
        caps.shim_capability().import_scope_with_options(
            roots,
            names.iter().map(|name| (*name).into()).collect(),
            lean_rs::module::HostImportOptions::default(),
        )
    }
}

#[allow(dead_code, reason = "host services only")]
pub(super) fn native_module_syntax_source_paths(
    caps: &LeanCapabilities<'_, '_>,
    session: &lean_rs_host::LeanSession<'_, '_>,
    names: &[&str],
) -> Vec<String> {
    let (_, imports) = session.source_context();
    imports
        .check_capability(caps.shim_capability())
        .expect("same loaded capability");
    assert!(imports.matches_request(
        &caps.import_search_paths(),
        &names.iter().map(|name| (*name).to_owned()).collect::<Vec<_>>()
    ));
    assert_eq!(imports.options(), &lean_rs::module::HostImportOptions::default());
    native_module_syntax_paths(caps, names, true)
}

fn native_module_syntax_paths(caps: &LeanCapabilities<'_, '_>, names: &[&str], source: bool) -> Vec<String> {
    assert!(matches!(
        names,
        ["Fixture.Imported", "Fixture.Internal", "Fixture.PrivateScope"]
            | ["Fixture.Internal", "Fixture.PrivateScope", "Fixture.Imported"]
    ));
    if source {
        native_source_paths(caps, names).expect("ordinary module syntax fixture selected")
    } else {
        #[cfg(all(test, unix))]
        {
            native_import_paths(caps, names).expect("ordinary module syntax fixture selected")
        }
        #[cfg(not(all(test, unix)))]
        panic!("ordinary module syntax fixture selected")
    }
}

fn native_source_paths(caps: &LeanCapabilities<'_, '_>, modules: &[&str]) -> Option<Vec<String>> {
    #[cfg(all(test, unix))]
    {
        native_import_paths(caps, modules).map(|mut paths| {
            paths.push(
                Path::new(lean_toolchain::LEAN_HEADER_PATH)
                    .ancestors()
                    .nth(3)
                    .expect("build-selected Lean prefix")
                    .join("lib/lean")
                    .to_string_lossy()
                    .into_owned(),
            );
            paths
        })
    }
    #[cfg(not(all(test, unix)))]
    {
        let _ = (caps, modules);
        None
    }
}

#[allow(
    dead_code,
    reason = "this fixture entry is shared by separate test and profiling binaries"
)]
pub(super) fn curated_theorem<'lean, 'c>(
    caps: &'c LeanCapabilities<'lean, '_>,
    session: &lean_rs_host::LeanSession<'lean, '_>,
) -> lean_rs::host_bridge::TrustedHostKernelRequest<'lean, 'c> {
    let (env, imports) = session.source_context();
    let elaboration_only =
        imports.matches_request(&caps.import_search_paths(), &["LeanRsHostShims.Elaboration".into()]);
    let theorem = if elaboration_only {
        "theorem lean_rs_curated : 1 + 1 = 2 := rfl"
    } else {
        assert!(imports.matches_request(
            &caps.import_search_paths(),
            &["LeanRsFixture.Handles".into(), "LeanRsHostShims.Meta".into()]
        ));
        "theorem lean_rs_session_workflow_rfl : 1 + 1 = 2 := rfl"
    };
    assert_eq!(imports.options(), &lean_rs::module::HostImportOptions::default());
    let paths = if elaboration_only && native_fixture_selected() {
        native_elaboration_paths(caps, session)
    } else {
        resolution()
    };
    #[allow(unsafe_code, reason = "one fixed reflexivity theorem in the admitted test context")]
    // SAFETY: the exact source and default settings use the pinned elaborator and kernel.
    unsafe {
        caps.shim_capability().kernel_scope(
            env,
            imports,
            paths,
            theorem.into(),
            lean_rs_host::LeanElabOptions::new().source_options(),
        )
    }
    .expect("attest curated theorem")
}
pub(super) fn project_library() -> lean_rs::module::LeanLibrary<'static> {
    let a = admission();
    let m = fixture_root().join(".lake/qualification-manifest.json");
    assert!(a.record.artifact_sha256.contains_key(&m));
    let desc: serde_json::Value =
        serde_json::from_slice(&fs::read(m).expect("held fixture manifest")).expect("approved manifest");
    let path = Path::new(
        desc.get("primary_dylib")
            .and_then(serde_json::Value::as_str)
            .expect("fixture image"),
    );
    assert!(a.record.artifact_sha256.contains_key(path) && path.starts_with(fixture_root()));
    #[allow(unsafe_code, reason = "fixed fixture image under lifetime custody")]
    // SAFETY: the controlled record covers this image and dependency closure under the held domains.
    let load = || {
        unsafe { lean_rs::module::LeanLibrary::open(LeanRuntime::init().expect("paired runtime"), path) }
            .expect("approved fixture library")
    };
    static PROJECT_RETAINED: OnceLock<()> = OnceLock::new();
    PROJECT_RETAINED.get_or_init(|| {
        Box::leak(Box::new(load()));
    });
    load()
}

#[allow(
    dead_code,
    reason = "this fixture entry is shared by separate test and profiling binaries"
)]
pub(super) fn term_batch<'lean, 'c>(
    caps: &'c LeanCapabilities<'lean, '_>,
    session: &lean_rs_host::LeanSession<'lean, '_>,
    cases: &[&str],
) -> lean_rs::host_bridge::TrustedHostTermBatch<'lean, 'c> {
    term_batch_for(caps, session, cases, false)
}

#[allow(dead_code, reason = "shared with separate strict test binaries")]
pub(super) fn native_term_batch<'lean, 'c>(
    caps: &'c LeanCapabilities<'lean, '_>,
    session: &lean_rs_host::LeanSession<'lean, '_>,
    cases: &[&str],
) -> lean_rs::host_bridge::TrustedHostTermBatch<'lean, 'c> {
    term_batch_for(caps, session, cases, true)
}

fn term_batch_for<'lean, 'c>(
    caps: &'c LeanCapabilities<'lean, '_>,
    session: &lean_rs_host::LeanSession<'lean, '_>,
    cases: &[&str],
    ordinary: bool,
) -> lean_rs::host_bridge::TrustedHostTermBatch<'lean, 'c> {
    assert!(cases.is_empty() || cases == ["(1 + 2 : Nat)", "1 +", "(1 + \"hi\" : Nat)"]);
    let (env, imports) = session.source_context();
    assert!(imports.matches_request(&caps.import_search_paths(), &["LeanRsHostShims.Elaboration".into()]));
    assert_eq!(imports.options(), &lean_rs::module::HostImportOptions::default());
    let paths = ordinary
        .then(|| native_source_paths(caps, &["LeanRsHostShims.Elaboration"]))
        .flatten()
        .unwrap_or_else(resolution);
    #[allow(
        unsafe_code,
        reason = "exact source-reviewed test batches in the admitted fixture environment"
    )]
    // SAFETY: the listed batch contains only the valid, parser-error and type-error expressions
    // reviewed for this pinned default elaborator and the exact retained search roots.
    unsafe {
        caps.shim_capability().term_batch_scope(
            env,
            imports,
            paths,
            cases.iter().map(|s| (*s).to_owned()).collect(),
            lean_rs_host::LeanElabOptions::new().source_options(),
        )
    }
    .expect("attest batch")
}
