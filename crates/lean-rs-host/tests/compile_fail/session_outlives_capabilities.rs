//! Returning the session would let its borrow outlive its owner.

use lean_rs::module::TrustedHostImports;
use lean_rs_host::{LeanCapabilities, LeanSession};

fn escape<'a>(caps: LeanCapabilities<'static, 'a>, imports: &TrustedHostImports<'static, 'a>) -> LeanSession<'static, 'a> {
    caps.session(imports, None, None).expect("session")
}

fn main() {}
