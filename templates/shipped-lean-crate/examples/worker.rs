fn main() -> Result<(), Box<dyn std::error::Error>> {
    let spec = lean_rs::LeanBuiltCapability::path(env!("LEAN_RS_CAPABILITY_SHIP_LEAN_DEMO_DYLIB"))
        .env_var("LEAN_RS_CAPABILITY_SHIP_LEAN_DEMO_DYLIB")
        .package("ship_lean_demo")
        .module("ShipLeanDemo");
    let mut capability = lean_rs_worker::LeanWorkerCapabilityBuilder::from_built_capability(
        &spec,
        ["ShipLeanDemo"],
    )?
    .worker_child(
        lean_rs_worker::LeanWorkerChild::sibling("shipped-lean-crate-worker")
            .env_override("SHIPPED_LEAN_CRATE_WORKER"),
    )
    .open()?;

    let _session = capability.open_session(None, None)?;
    println!("worker capability opened");
    Ok(())
}
