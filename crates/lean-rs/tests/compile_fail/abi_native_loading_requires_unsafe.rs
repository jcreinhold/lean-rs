use lean_rs::module::{LeanLibrary, LeanLibraryBundle};
use lean_rs::{LeanBuiltCapability, LeanCapability, LeanRuntime};

fn load(runtime: &LeanRuntime, spec: LeanBuiltCapability) {
    let _ = LeanLibrary::open(runtime, "native.dylib");
    let _ = LeanLibrary::open_globally(runtime, "native.dylib");
    let _ = LeanLibraryBundle::open(runtime, "native.dylib", []);
    let _ = LeanCapability::from_build_manifest(runtime, spec);
}

fn main() {}
