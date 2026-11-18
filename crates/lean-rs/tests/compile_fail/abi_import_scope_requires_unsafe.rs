use lean_rs::module::TrustedHostCapability;

fn attest_imports(capability: &TrustedHostCapability<'_>) {
    let _ = capability.import_scope(vec![], vec![]);
}

fn main() {}
