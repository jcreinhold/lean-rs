#[path = "src/support/deployment.rs"]
mod deployment;
fn main() -> Result<(), Box<dyn std::error::Error>> {
    if let Some(out) = std::env::var_os("OUT_DIR") {
        // The worker reads manifests under the managed package locks. A Cargo
        // build script normally emits them under OUT_DIR, outside those domains.
        // Run the same preparation in a child with the SDK's workspace-local
        // manifest layout; Cargo directives are inherited on stdout.
        let record = std::path::PathBuf::from(out).join("worker-deployment.json");
        let status = std::process::Command::new(std::env::current_exe()?)
            .env_remove("OUT_DIR")
            .arg(record)
            .status()?;
        return if status.success() {
            Ok(())
        } else {
            Err(format!("package preparation failed: {status}").into())
        };
    }
    let path = std::path::PathBuf::from(std::env::args_os().nth(1).ok_or("missing worker deployment output")?);
    use lean_toolchain::{
        LeanExportAbiRepr, LeanExportArgAbi, LeanExportOwnership, LeanExportResultConvention, LeanExportReturnAbi,
        LeanExportSignature,
    };

    let owner = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR")?);
    let lean_root = owner.join("lean");
    let bootstrap = lean_toolchain::shared_writer::PrivateLakeBootstrap::new(&owner, &lean_root)?;
    let demo = lean_toolchain::CargoLeanCapability::new(&lean_root, "ShipLeanDemo")
        .private_bootstrap(bootstrap)
        .package("ship_lean_demo")
        .module("ShipLeanDemo")
        .export_signature(LeanExportSignature::function(
            "ship_lean_demo_add",
            vec![
                LeanExportArgAbi::new(LeanExportAbiRepr::U64, LeanExportOwnership::None),
                LeanExportArgAbi::new(LeanExportAbiRepr::U64, LeanExportOwnership::None),
            ],
            LeanExportReturnAbi::new(
                LeanExportAbiRepr::U64,
                LeanExportOwnership::None,
                LeanExportResultConvention::Pure,
            ),
        ))
        .build()?;
    if !deployment::matches_demo(
        &serde_json::from_slice(&std::fs::read(demo.manifest_path())?)?,
        demo.dylib_path(),
    ) {
        return Err("unexpected packaged image or initializer".into());
    }
    // Resolve the host dependency from Cargo's selected graph. The build script
    // needs static ABI metadata and Lake only; it must not link the Lean runtime.
    let metadata = std::process::Command::new(std::env::var_os("CARGO").ok_or("missing Cargo")?)
        .args(["metadata", "--locked", "--offline", "--format-version", "1"])
        .arg("--filter-platform")
        .arg(std::env::var("TARGET")?)
        .output()?;
    if !metadata.status.success() {
        return Err(String::from_utf8_lossy(&metadata.stderr).into_owned().into());
    }
    let graph: serde_json::Value = serde_json::from_slice(&metadata.stdout)?;
    let root = &graph["resolve"]["root"];
    let node = graph["resolve"]["nodes"]
        .as_array()
        .ok_or("missing graph")?
        .iter()
        .find(|n| &n["id"] == root)
        .ok_or("missing root")?;
    let dep = node["deps"]
        .as_array()
        .ok_or("missing dependencies")?
        .iter()
        .find(|d| d["name"] == "lean_rs_host")
        .ok_or("missing host dependency")?;
    let host = graph["packages"]
        .as_array()
        .ok_or("missing packages")?
        .iter()
        .find(|p| p["id"] == dep["pkg"])
        .ok_or("missing host package")?;
    let host = std::path::Path::new(host["manifest_path"].as_str().ok_or("missing host manifest")?)
        .parent()
        .ok_or("missing package root")?;
    let interop = host.join("shims/lean-rs-interop-shims");
    let shim = host.join("shims/lean-rs-host-shims");
    let sysroot = lean_toolchain::discover_toolchain(&Default::default())?
        .prefix
        .canonicalize()?;
    let builder = |root: &std::path::Path, package: &str, target: &str| -> Result<_, Box<dyn std::error::Error>> {
        let private = lean_toolchain::shared_writer::PrivateLakeBootstrap::new(host, root)?;
        Ok(lean_toolchain::CargoLeanCapability::new(root, target)
            .package(package)
            .module(target)
            .lean_sysroot(&sysroot)
            .private_bootstrap(private))
    };
    builder(&interop, "lean_rs_interop_shims", "LeanRsInterop")?.build_quiet()?;
    let mut host_builder = builder(&shim, "lean_rs_host_shims", "LeanRsHostShims")?;
    for sig in lean_toolchain::host_shim_abi::compiled_host_binding_signatures().into_values() {
        host_builder = host_builder.export_signature(sig);
    }
    let built = host_builder.build_quiet()?;
    let shim_roots = lean_toolchain::shared_writer::project_artifact_root_union(&[shim.clone(), interop.clone()])?;
    let mut roots = shim_roots.clone();
    roots.push(lean_root.clone());
    let _readers = deployment::hold(&roots)?;
    let mut record = deployment::Deployment {
        project: lean_root.canonicalize()?,
        sysroot,
        shim_manifest: built.manifest_path().canonicalize()?,
        shim_paths: vec![
            interop.join(".lake/build/lib/lean").canonicalize()?,
            shim.join(".lake/build/lib/lean").canonicalize()?,
        ],
        shim_roots,
        files: Default::default(),
    };
    record.files = record.inventory()?;
    std::fs::write(&path, serde_json::to_vec(&record)?)?;
    println!("cargo:rustc-env=SHIP_DEMO_DEPLOYMENT={}", path.display());
    Ok(())
}
