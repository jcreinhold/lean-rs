fn main() {
    let prefix = std::env::var("LEAN_SYSROOT").expect("private build-selected Lean release");
    println!("cargo:rerun-if-env-changed=LEAN_SYSROOT");
    println!("cargo:rustc-link-arg=-Wl,-rpath,{prefix}/lib/lean");
}
