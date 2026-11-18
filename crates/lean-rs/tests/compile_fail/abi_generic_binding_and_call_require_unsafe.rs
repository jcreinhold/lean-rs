use lean_rs::{LeanCapability, LeanExported};

fn bind(capability: &LeanCapability<'_>) {
    let _ = capability.exported::<(String,), String>("native_string");
}

fn invoke(handle: &LeanExported<'_, '_, (String,), String>) {
    let _ = handle.call(String::new());
}

fn main() {}
