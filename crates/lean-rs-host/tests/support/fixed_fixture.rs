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
#[allow(
    clippy::option_env_unwrap,
    reason = "unprovisioned builds must compile and fail closed when qualification is attempted"
)]
fn admission() -> &'static Admission {
    static INSTANCE: OnceLock<Admission> = OnceLock::new();
    INSTANCE.get_or_init(|| {
        let record: Record = serde_json::from_str(
            option_env!("LEAN_RS_TEST_REVIEWED_RECORD")
                .expect("qualification build requires a protected fixture record"),
        )
        .expect("valid fixture record");
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
            Some(text_digest(option_env!("LEAN_RS_TEST_REVIEWED_RECORD").expect("selected fixture")).as_str()),
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
        let source = option_env!("LEAN_RS_TEST_SOURCE_SHA256").expect("reviewed source closure identity");
        assert!(source.len() == 64 && source.bytes().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(source, review.source_sha256);
        let inventory_path = roots[4].join("release-inventory.json");
        assert_eq!(
            inventory_path.canonicalize().expect("held release inventory"),
            inventory_path
        );
        let expected = option_env!("LEAN_RS_TEST_RELEASE_INVENTORY_SHA256")
            .expect("authenticated transitive release inventory digest");
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
        let graph_policy = option_env!("LEAN_RS_TEST_APPROVED_GRAPHS").expect("independent graph and request policy");
        assert_eq!(text_digest(graph_policy), review.graph_policy_sha256);
        let approved_rows = serde_json::from_str(graph_policy).expect("valid approved rows");
        for (p, h) in &record.artifact_sha256 {
            assert_eq!(p.canonicalize().expect("canonical fixture input"), *p);
            assert!(p.starts_with(&record.prefix) || roots.iter().any(|root| p.starts_with(root)));
            assert_eq!(digest(p), *h, "input changed: {}", p.display());
        }
        // Include every file in each ordered import directory (including sidecars).
        // Unlisted new oleans could otherwise shadow an admitted transitive import.
        let mut inventory: BTreeMap<PathBuf, String> = serde_json::from_str(
            option_env!("LEAN_RS_TEST_REVIEWED_IMPORT_FILES")
                .expect("qualification build requires a complete reviewed import inventory"),
        )
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
    let interop = record.scratch.join("lean-rs-interop-shims");
    let manifest = host_root.join(".lake/qualification-manifest.json");
    assert!(record.artifact_sha256.contains_key(&manifest));
    let manifest_json =
        fs::read_to_string(&manifest).map_err(|err| lean_rs::error::host_module_init(err.to_string()))?;
    let roots = vec![host_root, interop];
    let shim_paths = paths().into_iter().skip(1).collect::<Vec<_>>();
    let prepared = LeanPreparedHostArtifacts::from_snapshot(
        manifest.clone(),
        manifest_json.clone(),
        roots.clone(),
        shim_paths.clone(),
    )?;
    let rt = LeanRuntime::init()?;
    #[allow(
        unsafe_code,
        reason = "fixed qualified snapshot; the launching harness pins the actual runtime and artifact closure"
    )]
    // SAFETY: admission verified the reviewed image and manifest bytes under the fixture
    // loans. The protected record approves these exact init symbols and visibility.
    // Keep a permanent image reference for Lean's process-global registrations.
    let shim = unsafe { LoadedHostArtifacts::load(rt, manifest, manifest_json.into_bytes(), roots, shim_paths, ())? };
    if !load_project {
        return LeanCapabilities::from_prepared(host, &prepared, shim);
    }
    let root_manifest = fixture_root().join(".lake/qualification-manifest.json");
    let project = host
        .project_library_path("lean_rs_fixture", "LeanRsFixture")
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
    assert!(matches!(
        names,
        ["LeanRsFixture.Handles"
            | "LeanRsFixture.SourceRanges"
            | "LeanRsHostShims.Elaboration"
            | "LeanRsHostShims.Meta"
            | "LeanRsHostShims.InfoTree"
            | "LeanRsFixture.Meta"
            | "LeanRsFixtureLateExtension"]
            | ["LeanRsFixture.Handles", "LeanRsHostShims.Meta"]
    ));
    assert!(
        profile == lean_rs_host::LeanSessionImportProfile::Private
            || (names == ["LeanRsFixture.Handles"]
                && profile == lean_rs_host::LeanSessionImportProfile::FullPrivateCompat)
    );
    for name in names {
        let relative = PathBuf::from(name.replace('.', "/")).with_extension("olean");
        assert!(
            paths().iter().any(|root| root.join(&relative).is_file()),
            "missing admitted direct module {name}"
        );
    }
    let options = lean_rs_host::LeanImportProfileMode::FullSession(profile)
        .native_options(&lean_rs_host::LeanImportProfilerOptions::default());
    let roots = approved_graph(caps, names, &options, "session");
    #[allow(
        unsafe_code,
        reason = "two fixed source-reviewed imports in the admitted ordered fixture and release closure"
    )]
    // SAFETY: these fixed fixture graphs have been approved with the protected inventory,
    // held directories and pinned release. Bind this particular capability and exact roots.
    unsafe {
        caps.shim_capability().import_scope_with_options(
            roots,
            names.iter().map(|n| (*n).to_owned()).collect(),
            lean_rs_host::LeanImportProfileMode::FullSession(profile)
                .native_options(&lean_rs_host::LeanImportProfilerOptions::default()),
        )
    }
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
#[allow(
    clippy::panic,
    reason = "the test fixture must reject every unlisted source expression"
)]
pub(super) fn term<'lean, 'c>(
    caps: &'c LeanCapabilities<'lean, '_>,
    session: &lean_rs_host::LeanSession<'lean, '_>,
    source: &str,
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
            resolution(),
            source.to_owned(),
            None,
            lean_rs_host::LeanElabOptions::new().source_options(),
        )
    }
    .expect("attest fixture term")
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
    let theorem = if imports.matches_request(&caps.import_search_paths(), &["LeanRsHostShims.Elaboration".into()]) {
        "theorem lean_rs_curated : 1 + 1 = 2 := rfl"
    } else {
        assert!(imports.matches_request(
            &caps.import_search_paths(),
            &["LeanRsFixture.Handles".into(), "LeanRsHostShims.Meta".into()]
        ));
        "theorem lean_rs_session_workflow_rfl : 1 + 1 = 2 := rfl"
    };
    assert_eq!(imports.options(), &lean_rs::module::HostImportOptions::default());
    #[allow(unsafe_code, reason = "one fixed reflexivity theorem in the admitted test context")]
    // SAFETY: the exact source and default settings use the pinned elaborator and kernel.
    unsafe {
        caps.shim_capability().kernel_scope(
            env,
            imports,
            resolution(),
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
    assert!(cases.is_empty() || cases == ["(1 + 2 : Nat)", "1 +", "(1 + \"hi\" : Nat)"]);
    let (env, imports) = session.source_context();
    assert!(imports.matches_request(&caps.import_search_paths(), &["LeanRsHostShims.Elaboration".into()]));
    assert_eq!(imports.options(), &lean_rs::module::HostImportOptions::default());
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
            resolution(),
            cases.iter().map(|s| (*s).to_owned()).collect(),
            lean_rs_host::LeanElabOptions::new().source_options(),
        )
    }
    .expect("attest batch")
}
