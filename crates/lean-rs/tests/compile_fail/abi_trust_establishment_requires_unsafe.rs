use lean_rs::LeanCapability;
use lean_rs::module::{TrustedHostCapability, TrustedWorkerCapability};

fn trust_host(capability: LeanCapability<'_>) {
    let _ = TrustedHostCapability::from_loaded(capability);
}

fn trust_worker(capability: LeanCapability<'_>) {
    let _ = TrustedWorkerCapability::from_loaded(capability, []);
}

fn approve_worker(runtime: &lean_rs::LeanRuntime) {
    let _ = lean_rs::module::AuthorizedWorkerSnapshot::attest(runtime, "manifest".into(), vec![], vec![], ());
}

fn main() {}
