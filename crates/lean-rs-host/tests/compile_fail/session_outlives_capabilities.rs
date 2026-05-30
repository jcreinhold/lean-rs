//! A `LeanSession<'lean, 'c>` borrows its parent `LeanCapabilities` for `'c`
//! (`LeanCapabilities::session(&'c self, ...)`). The session owns the imported
//! environment but reuses the capability's checked shim bindings, so it cannot
//! outlive that borrow. Here the capabilities (and the host they borrow) are
//! locals that drop at the end of the inner block while the session escapes to
//! the outer scope—the borrow checker must reject the escape.
//!
//! The constructors compile but never run (this is a compile-fail test); the
//! `/tmp/nowhere` project path is never opened.

use lean_rs::LeanRuntime;
use lean_rs_host::{LeanHost, LeanSession};

fn main() {
    let session: LeanSession<'_, '_>;
    {
        let runtime: &'static LeanRuntime = LeanRuntime::init().expect("runtime init");
        let host = LeanHost::from_lake_project(runtime, "/tmp/nowhere").expect("host");
        let caps = host.load_shims_only().expect("capabilities");
        session = caps.session(&[], None, None).expect("session");
    }
    // `session` borrows `caps`, which dropped at the end of the inner block.
    let _escape = session;
}
