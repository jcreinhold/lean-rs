//! A `PooledSession<'lean, 'p, 'c>` borrows the `SessionPool` it was acquired
//! from for `'p` (`SessionPool::acquire(&'p self, ...)`); on drop it returns
//! its environment to that pool. It therefore cannot outlive the pool. Here
//! the pool is a local that drops at the end of the inner block while the
//! pooled session escapes to the outer scope—the borrow checker must reject
//! the escape.
//!
//! The constructors compile but never run (this is a compile-fail test); the
//! `/tmp/nowhere` project path is never opened.

use lean_rs::LeanRuntime;
use lean_rs_host::{LeanHost, PooledSession, SessionPool};

fn main() {
    let runtime: &'static LeanRuntime = LeanRuntime::init().expect("runtime init");
    let host = LeanHost::from_lake_project(runtime, "/tmp/nowhere").expect("host");
    let caps = host.load_shims_only().expect("capabilities");

    let pooled: PooledSession<'_, '_, '_>;
    {
        let pool = SessionPool::with_capacity(runtime, 2);
        pooled = pool.acquire(&caps, &[], None, None).expect("acquire");
    }
    // `pooled` borrows `pool`, which dropped at the end of the inner block.
    let _escape = pooled;
}
