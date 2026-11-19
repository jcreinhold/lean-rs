use lean_rs_worker_protocol::worker_exports::metadata_signature;
use lean_toolchain::CargoLeanCapability;
use std::{
    env, fs,
    path::{Path, PathBuf},
};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let app = Path::new(env!("CARGO_MANIFEST_DIR"));
    let project = app.join("lean");
    let sysroot =
        PathBuf::from(env::var_os("LEAN_SYSROOT").ok_or("missing sysroot")?).canonicalize()?;
    let prepared = lean_rs_host::host::prepare_host_artifacts()?;
    let (typed_session, verification) = match env::args().skip(1).collect::<Vec<_>>().as_slice() {
        [] => (false, false),
        [mode] if mode == "--typed-session" => (true, false),
        [mode] if mode == "--verify-declaration" => (true, true),
        _ => return Err("expected no arguments, --typed-session or --verify-declaration".into()),
    };
    let builder = if typed_session {
        CargoLeanCapability::new(&project, "LeanRsFixture")
            .package("lean_rs_fixture")
            .module("LeanRsFixture")
    } else {
        CargoLeanCapability::new(&project, "LeanRsInteropConsumer")
            .package("lean_rs_interop_consumer")
            .module("LeanRsInteropConsumer")
            .export_signature(metadata_signature(
                "lean_rs_interop_consumer_worker_metadata",
            ))
    };
    let built = builder
        .lean_sysroot(&sysroot)
        .lean_num_threads(1)
        .build_quiet()?;
    let roots = lean_toolchain::shared_writer::project_artifact_root_union(&[
        project.clone(),
        app.parent()
            .unwrap()
            .join("sdk/crates/lean-rs-host/shims/lean-rs-host-shims"),
    ])?;
    let helper =
        PathBuf::from(env::var_os("LEAN_HOST_MCP_WRITER_EXECUTABLE").ok_or("missing writer")?);
    let _guards = lean_toolchain::shared_reader::SharedReadProxy::acquire_union(
        &helper,
        &roots,
        std::time::Duration::from_secs(15),
        &|| false,
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
    let exports = if typed_session {
        serde_json::json!([])
    } else {
        serde_json::json!([expected.to_json()])
    };
    if consumer["exports"] != exports {
        return Err("incorrect metadata ABI".into());
    }
    let host: serde_json::Value = serde_json::from_str(shim.manifest_json())?;
    let declared = host["exports"].as_array().ok_or("missing host exports")?;
    let expected = lean_toolchain::host_shim_abi::compiled_host_binding_signatures();
    if declared.len() != expected.len()
        || expected
            .values()
            .any(|sig| !declared.contains(&sig.to_json()))
    {
        return Err("host shim does not implement the build-selected SDK ABI".into());
    }
    let record = serde_json::json!({"verification":verification,"typed_session":typed_session,"project":project,"sysroot":sysroot,"consumer_manifest":built.manifest_path(),"consumer_image":built.dylib_path(),
        "shim_manifest":shim.manifest_path(),"shim_json":shim.manifest_json(),"shim_roots":shim.artifact_roots(),
        "shim_paths":shim.shim_search_paths(),"roots":roots,
        "consumer_json":fs::read_to_string(built.manifest_path())?});
    fs::write(
        app.join("prepared.json"),
        serde_json::to_vec_pretty(&record)?,
    )?;
    let release_lock = fs::File::options()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(app.parent().unwrap().join("release.lock"))?;
    release_lock.try_lock_shared()?;
    let status = std::process::Command::new("python3")
        .arg(app.join("seal.py"))
        .status()?;
    if !status.success() {
        return Err("deployment sealing failed".into());
    }
    Ok(())
}
