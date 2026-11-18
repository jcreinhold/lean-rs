use lean_rs::LeanCapability;
use lean_rs::module::{TrustedHostCapability, TrustedHostImports};

fn escape(capability: LeanCapability<'static>) -> TrustedHostImports<'static, 'static> {
    // This function is compile-only; its assumed attestation cannot extend the
    // Rust borrow of the local token to the runtime's lifetime.
    // SAFETY: the fixture assumes its supplied capability satisfies host trust.
    let trusted = unsafe { TrustedHostCapability::from_loaded(capability) };
    // SAFETY: the fixture assumes the exact empty import graph is admitted.
    unsafe { trusted.import_scope(vec![], vec![]) }
}

fn main() {}
