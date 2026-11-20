#[path = "../stream_cases.rs"]
mod stream_cases;

use lean_rs_worker_protocol::worker_exports::{json_command_signature, metadata_signature};
use lean_toolchain::{CAPABILITY_MANIFEST_SCHEMA_VERSION, CargoLeanCapability, ToolchainFingerprint};
use std::io::Write as _;
use std::{
    env, fs,
    os::unix::{
        fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
        net::SocketAddr,
    },
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
    let broken_import = match profile.as_deref() {
        None => false,
        Some("--typed-session" | "--interop-stream") if env::args().len() == 3 => false,
        Some("--broken-import" | "--host-builder-lazy" | "--host-builder-shims-only") if env::args().len() == 3 => true,
        _ => return Err("unknown metadata qualification profile".into()),
    };
    let typed_session = profile.as_deref() == Some("--typed-session");
    let stream = profile.as_deref() == Some("--interop-stream");
    let state_name = if stream {
        "interop-stream"
    } else if broken_import {
        "broken-import"
    } else if typed_session {
        "typed-session"
    } else {
        "ordinary"
    };
    let expected_state = target.join("metadata-qualification").join(state_name);
    if state != expected_state {
        return Err("deployment state is outside the ordinary metadata target".into());
    }
    let sysroot = PathBuf::from(env::var_os("LEAN_SYSROOT").ok_or("missing sysroot")?).canonicalize()?;
    let generated_output_key = lean_toolchain::generated_output_key_for_sysroot(&sysroot)?
        .ok_or("installed Lean release does not use a generated-output slot")?;
    if typed_session || stream {
        return prepare_typed_session(app, sdk, &state, &sysroot, generated_output_key, stream);
    }
    if broken_import {
        return prepare_broken_import(app, &state, &sysroot, generated_output_key, profile.as_deref());
    }
    let project = sdk.join("fixtures/metadata-qualification/lean");
    let socket_root = target.join("ws");
    let socket_metadata = fs::symlink_metadata(&socket_root)?;
    let effective_uid = rustix::process::geteuid().as_raw();
    if socket_root.canonicalize()? != socket_root
        || !socket_metadata.is_dir()
        || socket_metadata.uid() != effective_uid
        || socket_metadata.mode() & 0o777 != 0o700
    {
        return Err("ordinary IPC root is not canonical, private, and deployment-owned".into());
    }
    SocketAddr::from_pathname(socket_root.join("lean-read-000000000000000000000000/fd.sock"))?;
    SocketAddr::from_pathname(socket_root.join("lean-worker-000000000000000000000000/parent.sock"))?;
    let prepared = lean_rs_host::host::prepare_host_artifacts_for_generated_output(generated_output_key)?
        .with_project_generated_output_matching_shims()?;
    let builder = CargoLeanCapability::new(&project, "LeanRsInteropConsumer")
        .package("lean_rs_interop_consumer")
        .module("LeanRsInteropConsumer")
        .generated_output_key(generated_output_key)
        .export_signature(metadata_signature("lean_rs_interop_consumer_worker_metadata"));
    let built = builder.lean_sysroot(&sysroot).lean_num_threads(1).build_quiet()?;
    let roots = lean_toolchain::shared_writer::project_artifact_root_union(&[
        project.clone(),
        sdk.join("crates/lean-rs-host/shims/lean-rs-host-shims"),
    ])?;
    let helper = PathBuf::from(env::var_os("LEAN_HOST_MCP_WRITER_EXECUTABLE").ok_or("missing writer")?);
    let _guards = lean_toolchain::shared_reader::SharedReadProxy::acquire_union_with_ipc_root(
        &helper,
        &roots,
        std::time::Duration::from_secs(15),
        &|| false,
        &socket_root,
    )?;
    let shim = lean_rs_host::host::LeanPreparedHostArtifacts::from_snapshot(
        prepared.manifest_path().into(),
        fs::read_to_string(prepared.manifest_path())?,
        prepared.artifact_roots().to_vec(),
        prepared.shim_search_paths().to_vec(),
    )?;
    if shim.manifest_json() != prepared.manifest_json() {
        return Err("prepared manifest changed".into());
    }
    let consumer: serde_json::Value = serde_json::from_slice(&fs::read(built.manifest_path())?)?;
    let expected = metadata_signature("lean_rs_interop_consumer_worker_metadata");
    let exports = serde_json::json!([expected.to_json()]);
    if consumer["exports"] != exports {
        return Err("incorrect metadata ABI".into());
    }
    let host: serde_json::Value = serde_json::from_str(shim.manifest_json())?;
    let declared = host["exports"].as_array().ok_or("missing host exports")?;
    let expected = lean_toolchain::host_shim_abi::compiled_host_binding_signatures();
    if declared.len() != expected.len() || expected.values().any(|sig| !declared.contains(&sig.to_json())) {
        return Err("host shim does not implement the build-selected SDK ABI".into());
    }
    let record = serde_json::json!({
        "typed_session": false,
        "verification": false,
        "generated_output_key": generated_output_key,
        "project": project,
        "sysroot": sysroot,
        "consumer_manifest": built.manifest_path(),
        "consumer_image": built.dylib_path(),
        "shim_manifest": shim.manifest_path(),
        "shim_json": shim.manifest_json(),
        "shim_roots": shim.artifact_roots(),
        "shim_paths": shim.shim_search_paths(),
        "roots": roots,
        "consumer_json": fs::read_to_string(built.manifest_path())?,
    });
    fs::write(state.join("prepared.json"), serde_json::to_vec_pretty(&record)?)?;
    seal(app, &state)
}

fn prepare_typed_session(
    app: &Path,
    sdk: &Path,
    state: &Path,
    sysroot: &Path,
    key: &str,
    stream: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    // Reuse the same run's prepared shims. This path never invokes Lake or a writer.
    let raw = env::var("LEAN_RS_PREPARED_HOST_ARTIFACTS")?;
    if raw.len() > 64 * 1024 {
        return Err("prepared descriptor exceeds limit".into());
    }
    let selected: lean_rs_worker_protocol::types::LeanWorkerPreparedHostArtifacts = serde_json::from_str(&raw)?;
    if selected.protocol != 2 {
        return Err("unexpected prepared descriptor protocol".into());
    }
    let shim = lean_rs_host::host::LeanPreparedHostArtifacts::from_snapshot(
        selected.shim_manifest_path.into(),
        selected.shim_manifest_json,
        selected.artifact_roots.into_iter().map(PathBuf::from).collect(),
        selected.shim_search_paths.into_iter().map(PathBuf::from).collect(),
    )?;
    let expected_project = sdk.join(if stream {
        "fixtures/interop-worker"
    } else {
        "fixtures/lean"
    });
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
    let (image_dir, stem) = if stream {
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
    let lock = lean_toolchain::shared_writer::project_lock_path(&project)?;
    match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&lock)
    {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.into()),
    }
    // Never replace, truncate or unlink an existing owner's record. Held checks
    // this same lock identity and previous-writer closure under its real reader.
    let metadata = fs::symlink_metadata(&lock)?;
    if !metadata.is_file() || lock.canonicalize()? != lock || metadata.nlink() != 1 {
        return Err("invalid fixture project lock".into());
    }
    // The ordinary job already built this dependency-free image. Only the metadata
    // below is new; the app's existing seal and held read locks cover the image.
    let state_metadata = fs::symlink_metadata(state)?;
    if !state_metadata.is_dir()
        || state_metadata.uid() != rustix::process::geteuid().as_raw()
        || state_metadata.mode() & 0o7777 != 0o700
    {
        return Err("typed state is not a private job directory".into());
    }
    let fingerprint = ToolchainFingerprint::current();
    let manifest_path = state.join(if stream {
        "worker-stream-capability.json"
    } else {
        "worker-json-capability.json"
    });
    let shim_json: serde_json::Value = serde_json::from_str(shim.manifest_json())?;
    let (package, module, dependencies, signatures) = if stream {
        (
            "lean_rs_interop_consumer",
            "LeanRsInteropConsumer",
            shim_json["dependencies"].clone(),
            stream_cases::CASES
                .iter()
                .flat_map(|(_, exports)| exports.signatures())
                .map(|sig| sig.to_json())
                .collect::<Vec<_>>(),
        )
    } else {
        (
            "lean_rs_fixture",
            "LeanRsFixture.Effects",
            serde_json::json!([]),
            vec![json_command_signature("lean_rs_interop_consumer_worker_json_command").to_json()],
        )
    };
    let manifest = serde_json::to_string(&serde_json::json!({
        "schema_version": CAPABILITY_MANIFEST_SCHEMA_VERSION,
        "package": package,
        "module": module,
        "primary_dylib": image,
        "dependencies": dependencies,
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
        return Err("typed JSON capability manifest failed SDK preflight".into());
    }
    let mut roots = shim.artifact_roots().to_vec();
    roots.push(project.clone());
    roots.sort();
    let record = serde_json::json!({
        "broken_import": false, "typed_session": true, "stream": stream, "verification": false,
        "consumer_manifest": manifest_path, "consumer_json": manifest,
        "generated_output_key": key, "project": project, "sysroot": sysroot,
        "consumer_image": image,
        "shim_manifest": shim.manifest_path(), "shim_json": shim.manifest_json(),
        "shim_roots": shim.artifact_roots(), "shim_paths": shim.shim_search_paths(), "roots": roots,
    });
    fs::write(state.join("prepared.json"), serde_json::to_vec(&record)?)?;
    seal(app, state)
}

fn prepare_broken_import(
    app: &Path,
    state: &Path,
    sysroot: &Path,
    key: &str,
    profile: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    if env::var_os(lean_toolchain::shared_writer::SHARED_WRITER_EXECUTABLE_ENV).is_some() {
        return Err("private broken-import fixture must not inherit a writer executable".into());
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
    let lock = lean_toolchain::shared_writer::project_lock_path(&project)?;
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(lock)?;

    let shim = lean_rs_host::host::prepare_host_artifacts()?;
    let mut scopes = shim.artifact_roots().to_vec();
    scopes.push(project.clone());
    scopes.sort();
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
        "shim_roots": shim.artifact_roots(),
        "shim_paths": shim.shim_search_paths(),
        "roots": scopes,
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
