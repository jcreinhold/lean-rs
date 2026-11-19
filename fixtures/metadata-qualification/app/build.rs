fn main() {
    let prefix = std::env::var("LEAN_SYSROOT").expect("build-selected installed Lean release");
    println!("cargo:rerun-if-env-changed=LEAN_SYSROOT");
    println!("cargo:rustc-link-arg=-Wl,-rpath,{prefix}/lib/lean");
    for name in [
        "LEAN_RS_METADATA_DEPLOYMENT_RECORD",
        "LEAN_RS_METADATA_DEPLOYMENT_SHA256",
        "LEAN_RS_METADATA_AUDIT_PATH",
    ] {
        println!("cargo:rerun-if-env-changed={name}");
        if let Ok(value) = std::env::var(name) {
            println!("cargo:rustc-env={name}={value}");
        }
    }
}
