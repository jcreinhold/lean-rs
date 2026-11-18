use lean_rs::module::{TrustedWorkerCapability, WorkerExportContract};
use std::fs;

const SYMBOL: &str = "ship_lean_demo_add";
const MANIFEST: &[u8] = include_bytes!(env!("LEAN_RS_CAPABILITY_SHIP_LEAN_DEMO_MANIFEST"));
const IMAGE: &[u8] = include_bytes!(env!("LEAN_RS_CAPABILITY_SHIP_LEAN_DEMO_DYLIB"));

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut manifest: serde_json::Value = serde_json::from_slice(MANIFEST)?;
    if manifest["package"] != "ship_lean_demo"
        || manifest["module"] != "ShipLeanDemo"
        || !manifest["dependencies"].as_array().is_some_and(Vec::is_empty)
    {
        return Err("unexpected shipped capability manifest".into());
    }
    // An application-owned directory keeps later Lake builds from replacing
    // the selected image between preflight and dlopen.
    let mut directory = tempfile::Builder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        directory.permissions(fs::Permissions::from_mode(0o700));
    }
    let stage = directory.tempdir()?;
    let library = stage
        .path()
        .join(format!("ShipLeanDemo.{}", std::env::consts::DLL_EXTENSION));
    fs::write(&library, IMAGE)?;
    manifest["primary_dylib"] = serde_json::to_value(&library)?;
    let path = stage.path().join("capability.json");
    let bytes = serde_json::to_vec(&manifest)?;
    fs::write(&path, &bytes)?;
    let runtime = lean_rs::LeanRuntime::init()?;
    // SAFETY: these build-time bytes come from the package's own ShipLeanDemo
    // target and matching Lean runtime. The initializer is the target's own,
    // and ShipLeanDemo.lean defines SYMBOL as pure UInt64 × UInt64 → UInt64.
    // The trusted deployment supplies compatible runtime libraries. The fresh
    // private directory and loaded mappings remain owned throughout the call.
    let capability = unsafe {
        TrustedWorkerCapability::load_snapshot(
            runtime,
            path,
            bytes,
            [(SYMBOL.to_owned(), WorkerExportContract::U64Binary)],
            stage,
        )
    }?;
    let answer = capability.bind_binary(SYMBOL)?.call(40, 2)?;
    println!("answer={answer}");
    Ok(())
}
