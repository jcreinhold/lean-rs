fn main() -> Result<(), Box<dyn std::error::Error>> {
    let runtime = lean_rs::LeanRuntime::init()?;
    let capability = lean_rs::LeanCapability::from_build_env(
        runtime,
        lean_rs::LeanBuiltCapability::path(env!("LEAN_RS_CAPABILITY_SHIP_LEAN_DEMO_DYLIB"))
            .env_var("LEAN_RS_CAPABILITY_SHIP_LEAN_DEMO_DYLIB")
            .package("ship_lean_demo")
            .module("ShipLeanDemo"),
    )?;
    let module = capability.module()?;
    let add = module.exported::<(u64, u64), u64>("ship_lean_demo_add")?;
    let answer = add.call(40, 2)?;
    println!("answer={answer}");
    Ok(())
}
