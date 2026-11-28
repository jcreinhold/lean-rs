#[path = "../interop_cases.rs"]
mod interop_cases;

use lean_rs_worker_protocol::worker_exports::{fixture_mul_signature, fixture_panic_signature};
use lean_toolchain::{CAPABILITY_MANIFEST_SCHEMA_VERSION, CargoLeanCapability, ToolchainFingerprint};
use std::io::Write as _;
use std::{
    env, fs,
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let app = Path::new(env!("CARGO_MANIFEST_DIR"));
    let sdk = app
        .ancestors()
        .nth(3)
        .ok_or("fixture app is not under the repository root")?;
    let state = PathBuf::from(env::args_os().nth(1).ok_or("missing deployment state directory")?);
    if !state.is_absolute() || state.canonicalize()? != state {
        return Err("deployment state is missing or aliased".into());
    }
    let target = sdk.join("target").canonicalize()?;
    let profile = env::args().nth(2);
    if profile.as_deref() == Some("--shim-package") && env::args().len() == 3 {
        if state != target.join("metadata-qualification/shim-package") {
            return Err("--shim-package requires the private fixture package".into());
        }
        let prepared = lean_rs_host::host::prepare_host_artifacts()?;
        println!("{}", prepared.manifest_path().display());
        return Ok(());
    }
    let broken_import = match profile.as_deref() {
        Some("--typed-session" | "--interop" | "--scalar-entry" | "--optional-whnf") if env::args().len() == 3 => false,
        Some(
            "--broken-import"
            | "--host-builder-lazy"
            | "--host-builder-shims-only"
            | "--host-builder-transitive"
            | "--host-builder-good"
            | "--direct-host-broken-shared",
        ) if env::args().len() == 3 => true,
        _ => return Err("unknown metadata qualification profile".into()),
    };
    let interop = profile.as_deref() == Some("--interop");
    let scalar = profile.as_deref() == Some("--scalar-entry");
    let optional_whnf = profile.as_deref() == Some("--optional-whnf");
    let expected_state = if optional_whnf {
        app.join("target/optional-whnf")
    } else if scalar {
        app.join("target/scalar-entry")
    } else if interop {
        target.join("metadata-qualification/interop")
    } else if broken_import {
        target.join("metadata-qualification/broken-import")
    } else {
        target.join("metadata-qualification/typed-session")
    };
    if state != expected_state {
        return Err("deployment state is outside the ordinary metadata target".into());
    }
    let sysroot = PathBuf::from(env::var_os("LEAN_SYSROOT").ok_or("missing sysroot")?).canonicalize()?;
    let generated_output_key = lean_toolchain::generated_output_key_for_sysroot(&sysroot)?
        .ok_or("installed Lean release does not use a generated-output slot")?;
    if scalar {
        return prepare_scalar(app, &state, &sysroot, generated_output_key);
    }
    if broken_import {
        return prepare_broken_import(app, &state, &sysroot, generated_output_key, profile.as_deref());
    }
    prepare_typed_session(app, sdk, &state, &sysroot, generated_output_key, interop)
}

fn prepare_scalar(app: &Path, state: &Path, sysroot: &Path, key: &str) -> Result<(), Box<dyn std::error::Error>> {
    if env::var_os("OUT_DIR").is_some() {
        return Err("scalar fixture requires its ordinary cache path".into());
    }
    let project = state.join("lean");
    let metadata = fs::symlink_metadata(&project)?;
    if project.canonicalize()? != project
        || !metadata.is_dir()
        || metadata.uid() != rustix::process::geteuid().as_raw()
        || metadata.mode() & 0o7777 != 0o700
        || project.join(".lake").try_exists()?
    {
        return Err("scalar fixture must be a fresh private source tree".into());
    }
    let builder = || {
        CargoLeanCapability::new(&project, "LeanRsFixture")
            .package("lean_rs_fixture")
            .module("LeanRsFixture")
            .export_signature(fixture_mul_signature("lean_rs_fixture_u64_mul"))
            .export_signature(fixture_panic_signature("lean_rs_fixture_panic_unit"))
    };
    let built = builder().build_quiet()?;
    let warm = builder()
        .probe_ready()?
        .ok_or("scalar build did not publish a warm cache")?;
    if warm.manifest_path() != built.manifest_path() || warm.dylib_path() != built.dylib_path() {
        return Err("scalar cache selected a different artifact after build".into());
    }
    let shim = selected_host_artifacts()?;
    let record = serde_json::json!({
        "scalar_entry": true, "typed_session": false, "verification": false,
        "consumer_manifest": warm.manifest_path(), "consumer_json": fs::read_to_string(warm.manifest_path())?,
        "generated_output_key": key, "project": project, "sysroot": sysroot,
        "consumer_image": warm.dylib_path(),
        "shim_manifest": shim.manifest_path(), "shim_json": shim.manifest_json(),
        "shim_paths": shim.shim_search_paths(),
    });
    fs::write(state.join("prepared.json"), serde_json::to_vec(&record)?)?;
    seal(app, state)
}

fn selected_host_artifacts() -> Result<lean_rs_host::host::LeanPreparedHostArtifacts, Box<dyn std::error::Error>> {
    let raw = env::var(lean_rs_worker_protocol::PREPARED_HOST_ARTIFACTS_ENV)?;
    if raw.len() > 64 * 1024 {
        return Err("prepared descriptor exceeds limit".into());
    }
    let selected: lean_rs_worker_protocol::types::LeanWorkerPreparedHostArtifacts = serde_json::from_str(&raw)?;
    if selected.protocol != 1 {
        return Err("unexpected prepared descriptor protocol".into());
    }
    Ok(lean_rs_host::host::LeanPreparedHostArtifacts::from_snapshot(
        selected.shim_manifest_path.into(),
        selected.shim_manifest_json,
        selected.shim_search_paths.into_iter().map(PathBuf::from).collect(),
    )?)
}

fn prepare_optional_whnf_shims(
    state: &Path,
    sysroot: &Path,
    key: &str,
) -> Result<lean_rs_host::host::LeanPreparedHostArtifacts, Box<dyn std::error::Error>> {
    if env::var_os("OUT_DIR").is_some() {
        return Err("optional WHNF fixture requires its private build".into());
    }
    let metadata = fs::symlink_metadata(state)?;
    if !metadata.is_dir() || metadata.uid() != rustix::process::geteuid().as_raw() || metadata.mode() & 0o7777 != 0o700
    {
        return Err("optional WHNF state is not job-owned and private".into());
    }
    let bundle = state.join("shims");
    let host = bundle.join("lean-rs-host-shims");
    let interop = bundle.join("lean-rs-interop-shims");
    if [&bundle, &host, &interop]
        .into_iter()
        .any(|p| p.canonicalize().as_ref().ok() != Some(p))
        || host.join(".lake").try_exists()?
        || interop.join(".lake").try_exists()?
    {
        return Err("optional WHNF requires two unbuilt, private shim roots".into());
    }
    let mut builder = CargoLeanCapability::new(&host, "LeanRsHostShims")
        .package("lean_rs_host_shims")
        .module("LeanRsHostShims")
        .lean_sysroot(sysroot)
        .lean_num_threads(2)
        .generated_output_key(key);
    // Keep the real call shape. The checked loader must reach MissingSymbol,
    // not the fatal MissingSignatureMetadata branch.
    for signature in lean_toolchain::host_shim_abi::compiled_host_binding_signatures().into_values() {
        builder = builder.export_signature(signature);
    }
    let built = builder.build_quiet()?;
    let manifest = built.manifest_path().canonicalize()?;
    use std::io::Read as _;
    let mut bytes = Vec::new();
    fs::File::open(&manifest)?.take(64 * 1024 + 1).read_to_end(&mut bytes)?;
    let search = [interop, host].map(|root| root.join(".lake/lean-rs-generated").join(key).join("lib/lean"));
    Ok(lean_rs_host::host::LeanPreparedHostArtifacts::from_snapshot(
        manifest,
        String::from_utf8(bytes)?,
        search.into(),
    )?)
}

fn prepare_typed_session(
    app: &Path,
    sdk: &Path,
    state: &Path,
    sysroot: &Path,
    key: &str,
    interop: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let optional_whnf = state == app.join("target/optional-whnf");
    let shim = if optional_whnf {
        prepare_optional_whnf_shims(state, sysroot, key)?
    } else {
        // Reuse the same run's normal prepared shims.
        selected_host_artifacts()?
    };
    let expected_project = if interop {
        sdk.join("target/metadata-qualification/shim-package/fixtures/interop-worker")
    } else if optional_whnf {
        sdk.join("fixtures/lean")
    } else {
        let selected = PathBuf::from(env::var_os("LEAN_RS_TEST_TYPED_PROJECT").ok_or("missing typed fixture project")?);
        if selected != sdk.join("fixtures/lean") && selected != state.join("lean") {
            return Err("typed fixture project is outside the approved job roots".into());
        }
        selected
    };
    let project = expected_project.canonicalize()?;
    // The trusted job already built this exact checkout fixture once and keeps
    // it unchanged until tests exit. This is not protection from same-user writes.
    let project_metadata = fs::symlink_metadata(&project)?;
    if project != expected_project
        || !project_metadata.is_dir()
        || project_metadata.uid() != rustix::process::geteuid().as_raw()
        || project_metadata.mode() & 0o7777 != 0o700
    {
        return Err("typed fixture is not the job-owned private checkout fixture".into());
    }
    let (image_dir, stem) = if interop {
        (
            format!(".lake/lean-rs-generated/{key}/lib"),
            "lean__rs__interop__consumer_LeanRsInteropConsumer",
        )
    } else {
        (".lake/build/lib".into(), "lean__rs__fixture_LeanRsFixture")
    };
    let image = project
        .join(image_dir)
        .join(format!("lib{stem}.{}", env::consts::DLL_EXTENSION));
    if image.canonicalize()? != image || !fs::symlink_metadata(&image)?.is_file() {
        return Err("missing canonical built scalar image".into());
    }
    // The ordinary job already built this dependency-free image. Only the metadata
    // below is new; the app's seal covers the image.
    let state_metadata = fs::symlink_metadata(state)?;
    if !state_metadata.is_dir()
        || state_metadata.uid() != rustix::process::geteuid().as_raw()
        || state_metadata.mode() & 0o7777 != 0o700
    {
        return Err("typed state is not a private job directory".into());
    }
    let mut record = serde_json::json!({
        "broken_import": false, "typed_session": true, "interop": interop, "verification": false,
        "optional_whnf": optional_whnf,
        "generated_output_key": key, "project": project, "sysroot": sysroot,
        "consumer_image": image,
        "shim_manifest": shim.manifest_path(), "shim_json": shim.manifest_json(),
        "shim_paths": shim.shim_search_paths(),
    });
    // The typed tests open the fixture library through its initializer, so only
    // the interop tests read a capability manifest.
    if interop {
        let (manifest_path, manifest) = write_interop_manifest(state, sysroot, &shim, &image)?;
        record["consumer_manifest"] = serde_json::json!(manifest_path);
        record["consumer_json"] = manifest.into();
    }
    fs::write(state.join("prepared.json"), serde_json::to_vec(&record)?)?;
    seal(app, state)
}

/// Writes the capability manifest the interop tests open. It names every fixed
/// export an interop case may load, and the shim package's interop dependency.
fn write_interop_manifest(
    state: &Path,
    sysroot: &Path,
    shim: &lean_rs_host::host::LeanPreparedHostArtifacts,
    image: &Path,
) -> Result<(PathBuf, String), Box<dyn std::error::Error>> {
    let fingerprint = ToolchainFingerprint::current();
    let manifest_path = state.join(interop_cases::MANIFEST_NAME);
    let shim_json: serde_json::Value = serde_json::from_str(shim.manifest_json())?;
    let mut seen = std::collections::BTreeSet::new();
    let signatures = interop_cases::CASES
        .iter()
        .flat_map(|(_, exports)| exports.signatures())
        .filter(|signature| seen.insert(signature.symbol().to_owned()))
        .map(|sig| sig.to_json())
        .collect::<Vec<_>>();
    let manifest = serde_json::to_string(&serde_json::json!({
        "schema_version": CAPABILITY_MANIFEST_SCHEMA_VERSION,
        "package": "lean_rs_interop_consumer",
        "module": "LeanRsInteropConsumer",
        "target_name": "LeanRsInteropConsumer",
        "primary_dylib": image,
        "dependencies": shim_json["dependencies"],
        "build_toolchain": {"sysroot": sysroot},
        "toolchain_fingerprint": {
            "lean_version": fingerprint.lean_version,
            "resolved_version": fingerprint.resolved_version,
            "header_sha256": fingerprint.header_sha256,
        },
        "exports": signatures,
    }))?;
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&manifest_path)?
        .write_all(manifest.as_bytes())?;
    if !lean_toolchain::manifest_validation::check_static(&manifest_path).is_ok() {
        return Err("interop capability manifest failed SDK preflight".into());
    }
    Ok((manifest_path, manifest))
}

fn prepare_broken_import(
    app: &Path,
    state: &Path,
    sysroot: &Path,
    key: &str,
    profile: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    if profile == Some("--host-builder-transitive") {
        let project = state.join("lean-transitive");
        fs::DirBuilder::new().mode(0o700).create(&project)?;
        build_transitive_fixture(&project, sysroot)?;
        return seal_shims_only(
            app,
            state,
            sysroot,
            key,
            project,
            Some("shims_only_host_handle_imports_transitive_lake_package_oleans"),
        );
    }
    if matches!(profile, Some("--host-builder-good" | "--direct-host-broken-shared")) {
        let broken_shared = profile == Some("--direct-host-broken-shared");
        let project = state.join(if broken_shared {
            "lean-broken-shared"
        } else {
            "lean-good"
        });
        fs::DirBuilder::new().mode(0o700).create(&project)?;
        fs::write(project.join("lean-toolchain"), "leanprover/lean4:v4.36.0\n")?;
        fs::write(
            project.join("lakefile.lean"),
            if broken_shared {
                "import Lake\nopen Lake DSL\npackage demo_broken\nlean_lib Good\nlean_lib Broken\n"
            } else {
                "import Lake\nopen Lake DSL\npackage demo_shims\nlean_lib Good\n"
            },
        )?;
        fs::write(project.join("Good.lean"), "def goodValue : Nat := 41\n")?;
        if !std::process::Command::new(sysroot.join("bin/lake"))
            .args(["build", "Good"])
            .current_dir(&project)
            .stdin(std::process::Stdio::null())
            .status()?
            .success()
        {
            return Err("expected the selected Lean to build Good".into());
        }
        if broken_shared {
            fs::write(
                project.join("Broken.lean"),
                "theorem broken : True := sorry_that_doesnt_exist\n",
            )?;
            check_broken_shared_build(&project, sysroot)?;
        }
        return seal_shims_only(
            app,
            state,
            sysroot,
            key,
            project,
            Some(if broken_shared {
                "load_shims_only_succeeds_when_user_shared_facet_does_not_build"
            } else {
                "shims_only_session_opens_without_user_dylib"
            }),
        );
    }
    let (directory, package, umbrella, good, bad_target, case) = match profile {
        Some("--host-builder-lazy") => (
            "lean-lazy",
            "worker_host_lazy",
            "import Demo.Good\n",
            true,
            None,
            Some("a_host_handle_built_without_imports_opens_no_session"),
        ),
        Some("--host-builder-shims-only") => (
            "lean-shims-only",
            "worker_host_handle",
            "import Demo.Good\nimport Demo.Broken\n",
            true,
            Some("Demo:shared"),
            Some("shims_only_host_handle_skips_user_shared_build"),
        ),
        _ => (
            "lean",
            "worker_host_broken",
            "import Demo.Broken\n",
            false,
            Some("Demo.Broken"),
            None,
        ),
    };
    let project = state.join(directory);
    fs::DirBuilder::new().mode(0o700).create(&project)?;
    fs::create_dir(project.join("Demo"))?;
    fs::write(
        project.join("lakefile.lean"),
        format!("import Lake\nopen Lake DSL\npackage {package}\nlean_lib Demo\n"),
    )?;
    fs::write(project.join("lean-toolchain"), "leanprover/lean4:v4.36.0\n")?;
    fs::write(project.join("Demo.lean"), umbrella)?;
    // Keep the real build outcome and stream Lean's diagnostic without a log.
    let build = |target: &str| {
        std::process::Command::new(sysroot.join("bin/lake"))
            .args(["build", target])
            .current_dir(&project)
            .stdin(std::process::Stdio::null())
            .status()
    };
    if good {
        fs::write(project.join("Demo/Good.lean"), "def goodValue : Nat := 41\n")?;
        if !build("Demo.Good")?.success() {
            return Err("expected the selected Lean to build Demo.Good".into());
        }
    }
    if let Some(target) = bad_target {
        fs::write(
            project.join("Demo/Broken.lean"),
            "theorem broken : True := sorry_that_doesnt_exist\n",
        )?;
        if build(target)?.code() != Some(1) {
            return Err(format!("expected the selected Lean to reject {target}").into());
        }
    }
    fs::create_dir_all(project.join(".lake/build/lib/lean"))?;
    seal_shims_only(app, state, sysroot, key, project, case)
}

fn check_broken_shared_build(project: &Path, sysroot: &Path) -> Result<(), Box<dyn std::error::Error>> {
    use std::io::{Read as _, Write as _};
    const LIMIT: u64 = 16 * 1024;
    let (reader, writer) = std::io::pipe()?;
    let mut child = std::process::Command::new(sysroot.join("bin/lake"))
        .args(["--no-cache", "--fail-fast", "build", "Broken:shared"])
        .current_dir(project)
        .stdin(std::process::Stdio::null())
        .stdout(writer.try_clone()?)
        .stderr(writer)
        .spawn()?;
    let mut diagnostic = Vec::new();
    let read = reader.take(LIMIT + 1).read_to_end(&mut diagnostic);
    if read.is_err() || u64::try_from(diagnostic.len())? > LIMIT {
        let _ = child.kill();
        child.wait()?;
        return Err("Broken:shared diagnostic exceeded its bounded read".into());
    }
    let status = child.wait()?;
    std::io::stdout().write_all(&diagnostic)?;
    if status.code() != Some(1)
        || !String::from_utf8(diagnostic)?
            .lines()
            .any(|line| line.contains("Broken.lean:") && line.contains("Unknown identifier `sorry_that_doesnt_exist`"))
    {
        return Err("expected Broken:shared to fail on its exact Lean source error".into());
    }
    Ok(())
}

fn build_transitive_fixture(project: &Path, sysroot: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let dep = project.join(".lake/packages/dep");
    fs::create_dir_all(dep.join("Dep"))?;
    for root in [project, &dep] {
        fs::write(root.join("lean-toolchain"), "leanprover/lean4:v4.36.0\n")?;
    }
    fs::write(
        dep.join("lakefile.lean"),
        "import Lake\nopen Lake DSL\npackage dep\nlean_lib Dep\n",
    )?;
    fs::write(dep.join("Dep/Hello.lean"), "def Dep.hello : Nat := 41\n")?;
    fs::write(
        project.join("lakefile.lean"),
        "import Lake\nopen Lake DSL\npackage consumer\nrequire dep from \"./.lake/packages/dep\"\nlean_lib Consumer\n",
    )?;
    fs::write(
        project.join("Consumer.lean"),
        "import Dep.Hello\ndef Consumer.value : Nat := Dep.hello + 1\n",
    )?;
    fs::write(
        project.join("lake-manifest.json"),
        concat!(
            r#"{"version":"1.2.0","packagesDir":".lake/packages","packages":[{"type":"path","scope":"","name":"dep","#,
            r#""manifestFile":"lake-manifest.json","inherited":false,"dir":"./.lake/packages/dep","#,
            r#""configFile":"lakefile.lean"}],"name":"consumer","lakeDir":".lake"}"#,
        ),
    )?;
    for (root, target) in [(dep.as_path(), "Dep.Hello"), (project, "Consumer")] {
        if !std::process::Command::new(sysroot.join("bin/lake"))
            .args(["build", target])
            .current_dir(root)
            .stdin(std::process::Stdio::null())
            .status()?
            .success()
        {
            return Err(format!("expected the selected Lean to build {target}").into());
        }
    }
    Ok(())
}

fn seal_shims_only(
    app: &Path,
    state: &Path,
    sysroot: &Path,
    key: &str,
    project: PathBuf,
    case: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    let shim = selected_host_artifacts()?;
    let record = serde_json::json!({
        "broken_import": true,
        "host_builder_case": case,
        "typed_session": false,
        "verification": false,
        "generated_output_key": key,
        "project": project,
        "sysroot": sysroot,
        "shim_manifest": shim.manifest_path(),
        "shim_json": shim.manifest_json(),
        "shim_paths": shim.shim_search_paths(),
    });
    fs::write(state.join("prepared.json"), serde_json::to_vec_pretty(&record)?)?;
    seal(app, state)
}

fn seal(app: &Path, state: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let status = std::process::Command::new("python3")
        .arg(app.join("seal.py"))
        .arg(state)
        .status()?;
    if !status.success() {
        return Err("deployment sealing failed".into());
    }
    Ok(())
}
