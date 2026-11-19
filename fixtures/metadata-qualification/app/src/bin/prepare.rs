use lean_rs_worker_protocol::worker_exports::metadata_signature;
use lean_toolchain::CargoLeanCapability;
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
    let broken_import = match env::args().nth(2).as_deref() {
        None => false,
        Some("--broken-import") if env::args().len() == 3 => true,
        _ => return Err("unknown metadata qualification profile".into()),
    };
    let state_name = if broken_import { "broken-import" } else { "ordinary" };
    let expected_state = target.join("metadata-qualification").join(state_name);
    if state != expected_state {
        return Err("deployment state is outside the ordinary metadata target".into());
    }
    let sysroot = PathBuf::from(env::var_os("LEAN_SYSROOT").ok_or("missing sysroot")?).canonicalize()?;
    let generated_output_key = lean_toolchain::generated_output_key_for_sysroot(&sysroot)?
        .ok_or("installed Lean release does not use a generated-output slot")?;
    if broken_import {
        return prepare_broken_import(app, &state, &sysroot, generated_output_key);
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

fn prepare_broken_import(
    app: &Path,
    state: &Path,
    sysroot: &Path,
    key: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    if env::var_os(lean_toolchain::shared_writer::SHARED_WRITER_EXECUTABLE_ENV).is_some() {
        return Err("private broken-import fixture must not inherit a writer executable".into());
    }
    let project = state.join("lean");
    fs::DirBuilder::new().mode(0o700).create(&project)?;
    fs::create_dir(project.join("Demo"))?;
    fs::write(
        project.join("lakefile.lean"),
        "import Lake\nopen Lake DSL\npackage worker_host_broken\nlean_lib Demo\n",
    )?;
    fs::write(project.join("lean-toolchain"), "leanprover/lean4:v4.36.0\n")?;
    fs::write(project.join("Demo.lean"), "import Demo.Broken\n")?;
    fs::write(
        project.join("Demo/Broken.lean"),
        "theorem broken : True := sorry_that_doesnt_exist\n",
    )?;
    // Let the caller see the actual Lean error without buffering its output.
    // The checkout-local smoke runner supplies the outer process deadline.
    let bad = std::process::Command::new(sysroot.join("bin/lake"))
        .arg("build")
        .arg("Demo.Broken")
        .current_dir(&project)
        .stdin(std::process::Stdio::null())
        .status()?;
    if bad.code() != Some(1) {
        return Err("expected the selected Lean to reject Demo.Broken".into());
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
