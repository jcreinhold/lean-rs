fn main() -> Result<(), Box<dyn std::error::Error>> {
    lean_toolchain::CargoLeanCapability::new("lean", "ShipLeanDemo")
        .package("ship_lean_demo")
        .module("ShipLeanDemo")
        .build()?;
    Ok(())
}
