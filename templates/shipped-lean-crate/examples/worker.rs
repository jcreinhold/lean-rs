fn main() -> Result<(), Box<dyn std::error::Error>> {
    let spec =
        lean_rs::LeanBuiltCapability::manifest_path(env!("LEAN_RS_CAPABILITY_SHIP_LEAN_DEMO_MANIFEST"))
            .manifest_env_var("LEAN_RS_CAPABILITY_SHIP_LEAN_DEMO_MANIFEST");
    let builder = lean_rs_worker_parent::LeanWorkerCapabilityBuilder::from_built_capability(
        &spec,
        ["ShipLeanDemo"],
    )?
    .worker_child(
        lean_rs_worker_parent::LeanWorkerChild::sibling("shipped-lean-crate-worker")
            .env_override("SHIPPED_LEAN_CRATE_WORKER"),
    );
    let report = builder.check();
    if let Some(first) = report.first_error() {
        return Err(format!("worker bootstrap check failed: {}", first.message()).into());
    }
    let mut capability = builder.open()?;

    let _session = capability.open_session(None, None)?;
    println!("worker capability opened");
    Ok(())
}
