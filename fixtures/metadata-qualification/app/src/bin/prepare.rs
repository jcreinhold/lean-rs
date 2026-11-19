use lean_rs_worker_protocol::worker_exports::metadata_signature;
use lean_toolchain::CargoLeanCapability;
use std::{
    env, fs,
    os::unix::{fs::MetadataExt, net::SocketAddr},
    path::{Path, PathBuf},
};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let app = Path::new(env!("CARGO_MANIFEST_DIR"));
    let sdk = app
        .ancestors()
        .nth(3)
        .ok_or("fixture app is not under the repository root")?;
    let project = sdk.join("fixtures/metadata-qualification/lean");
    let state = PathBuf::from(env::args_os().nth(1).ok_or("missing deployment state directory")?);
    if !state.is_absolute() || state.canonicalize()? != state {
        return Err("deployment state is missing or aliased".into());
    }
    let target = sdk.join("target").canonicalize()?;
    let expected_state = target.join("metadata-qualification/ordinary");
    if state != expected_state {
        return Err("deployment state is outside the ordinary metadata target".into());
    }
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
    let sysroot = PathBuf::from(env::var_os("LEAN_SYSROOT").ok_or("missing sysroot")?).canonicalize()?;
    let generated_output_key = lean_toolchain::generated_output_key_for_sysroot(&sysroot)?
        .ok_or("installed Lean release does not use a generated-output slot")?;
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
    let record = serde_json::json!({"typed_session":false,"verification":false,
        "generated_output_key":generated_output_key,
        "project":project,"sysroot":sysroot,"consumer_manifest":built.manifest_path(),"consumer_image":built.dylib_path(),
        "shim_manifest":shim.manifest_path(),"shim_json":shim.manifest_json(),"shim_roots":shim.artifact_roots(),
        "shim_paths":shim.shim_search_paths(),"roots":roots,
        "consumer_json":fs::read_to_string(built.manifest_path())?});
    fs::write(state.join("prepared.json"), serde_json::to_vec_pretty(&record)?)?;
    let status = std::process::Command::new("python3")
        .arg(app.join("seal.py"))
        .arg(&state)
        .status()?;
    if !status.success() {
        return Err("deployment sealing failed".into());
    }
    Ok(())
}
