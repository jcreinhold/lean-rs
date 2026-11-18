//! Returning the session would let its borrow outlive its owner.

use lean_rs::LeanRuntime;
use lean_rs::module::TrustedHostImports;
use lean_rs_host::{LeanCapabilities, PooledSession, SessionPool};

fn escape<'a>(runtime: &'static LeanRuntime, caps: &'a LeanCapabilities<'static, 'a>, imports: &TrustedHostImports<'static, 'a>) -> PooledSession<'static, 'a, 'a> {
    let pool = SessionPool::with_capacity(runtime, 2);
    pool.acquire(caps, imports, None, None).expect("session")
}

fn main() {}
