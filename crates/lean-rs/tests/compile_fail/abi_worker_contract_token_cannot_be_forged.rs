use lean_rs::LeanCapability;
use lean_rs::module::TrustedWorkerCapability;

fn forge(capability: LeanCapability<'_>) {
    let _ = TrustedWorkerCapability {
        capability,
        contracts: Default::default(),
    };
}

fn main() {}
