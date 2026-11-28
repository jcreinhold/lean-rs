fn main() -> Result<(), Box<dyn std::error::Error>> {
    let project = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("lean");
    let builder = lean_rs_worker_parent::LeanWorkerHostHandleBuilder::shims_only(project, ["ShipLeanDemo"])
        .worker_child(
            lean_rs_worker_parent::LeanWorkerChild::sibling("shipped-lean-crate-worker")
                .env_override("SHIPPED_LEAN_CRATE_WORKER"),
        );
    let report = builder.check();
    if let Some(first) = report.first_error() {
        return Err(format!("worker bootstrap check failed: {}", first.message()).into());
    }
    let mut handle = builder.open()?;

    let _session = handle.open_session(None, None)?;
    println!("worker host session opened");
    Ok(())
}
