fn main() -> Result<(), Box<dyn std::error::Error>> {
    use lean_toolchain::{
        LeanExportAbiRepr, LeanExportArgAbi, LeanExportOwnership, LeanExportResultConvention, LeanExportReturnAbi,
        LeanExportSignature,
    };

    let owner = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR")?);
    let lean_root = owner.join("lean");
    let bootstrap = lean_toolchain::shared_writer::PrivateLakeBootstrap::new(&owner, &lean_root)?;
    lean_toolchain::CargoLeanCapability::new(&lean_root, "ShipLeanDemo")
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
    Ok(())
}
