#[path = "src/support/deployment.rs"]
mod deployment;
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use lean_toolchain::{
        LeanExportAbiRepr, LeanExportArgAbi, LeanExportOwnership, LeanExportResultConvention, LeanExportReturnAbi,
        LeanExportSignature,
    };

    let path = std::path::PathBuf::from(std::env::var_os("OUT_DIR").ok_or("missing OUT_DIR")?)
        .join("worker-deployment.json");
    let lean_root = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR")?).join("lean");
    let toolchain = lean_toolchain::discover_toolchain(&Default::default())?;
    let pin = std::fs::read_to_string(lean_root.join("lean-toolchain"))?;
    if pin.trim() != format!("leanprover/lean4:v{}", toolchain.version) {
        return Err(format!("template pins {}, selected Lean {}", pin.trim(), toolchain.version).into());
    }
    if toolchain.version != lean_toolchain::LEAN_VERSION {
        return Err(format!(
            "selected Lean {} differs from Cargo's Lean {}; set LEAN_SYSROOT to the pinned Lean prefix",
            toolchain.version,
            lean_toolchain::LEAN_VERSION,
        )
        .into());
    }
    let sysroot = toolchain.prefix.canonicalize()?;
    lean_toolchain::emit_lean_link_directives_checked()?;
    println!("cargo:rerun-if-env-changed=ELAN_TOOLCHAIN");
    println!(
        "cargo:rerun-if-changed={}",
        sysroot.join("include/lean/version.h").display()
    );
    for input in [
        "lakefile.lean",
        "lake-manifest.json",
        "lean-toolchain",
        "ShipLeanDemo.lean",
    ] {
        println!("cargo:rerun-if-changed={}", lean_root.join(input).display());
    }
    let shim_output_key = lean_toolchain::generated_output_key_for_sysroot(&sysroot)?;
    let demo = lean_toolchain::CargoLeanCapability::new(&lean_root, "ShipLeanDemo")
        .lean_sysroot(&sysroot)
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
        ));
    // Cargo can keep its build record after Lake's outputs are removed. Watch
    // both files, and reuse a ready manifest without rewriting it when Cargo
    // reruns because the outputs were just generated.
    let demo = match demo.probe_ready()? {
        Some(ready) => ready,
        None => demo.build_quiet()?,
    };
    for (key, input) in [
        (demo.manifest_env_var(), demo.manifest_path()),
        (demo.env_var(), demo.dylib_path()),
    ] {
        println!("cargo:rerun-if-changed={}", input.display());
        println!("cargo:rustc-env={key}={}", input.display());
    }
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
    let builder = |root: &std::path::Path, package: &str, target: &str| {
        let builder = lean_toolchain::CargoLeanCapability::new(root, target)
            .package(package)
            .module(target)
            .lean_sysroot(&sysroot);
        match shim_output_key {
            Some(key) => builder.generated_output_key(key),
            None => builder,
        }
    };
    let interop_built = builder(&interop, "lean_rs_interop_shims", "LeanRsInterop").build_quiet()?;
    let mut host_builder = builder(&shim, "lean_rs_host_shims", "LeanRsHostShims");
    for sig in lean_toolchain::host_shim_abi::compiled_host_binding_signatures().into_values() {
        host_builder = host_builder.export_signature(sig);
    }
    let built = host_builder.build_quiet()?;
    // Both shim search paths must name the outputs this build actually selected.
    // The demo retains its standard Lake layout.
    let import_path = |built: &lean_toolchain::BuiltLeanCapability| -> Result<_, Box<dyn std::error::Error>> {
        Ok(built
            .dylib_path()
            .parent()
            .ok_or("missing shim library directory")?
            .join("lean")
            .canonicalize()?)
    };
    let mut record = deployment::Deployment {
        project: lean_root.canonicalize()?,
        sysroot,
        shim_manifest: built.manifest_path().canonicalize()?,
        shim_paths: vec![import_path(&interop_built)?, import_path(&built)?],
        files: Default::default(),
    };
    record.files = record.inventory()?;
    std::fs::write(&path, serde_json::to_vec(&record)?)?;
    println!("cargo:rustc-env=SHIP_DEMO_DEPLOYMENT={}", path.display());
    Ok(())
}
