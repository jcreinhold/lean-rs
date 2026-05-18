//! A `LeanHost<'a>` constructed from a non-`'static` runtime borrow
//! cannot outlive that borrow. We force the host's `'lean` parameter to
//! match a stack-local `()` via a function pointer that constrains both
//! lifetimes to the same `'a`, then try to escape the inner scope with
//! the host alive. The borrow checker must reject the assignment.

use lean_rs::LeanRuntime;
use lean_rs_host::LeanHost;

fn main() {
    let host: LeanHost<'_>;
    {
        let local = ();
        let runtime: &'static LeanRuntime =
            LeanRuntime::init().expect("runtime init");

        // Going through this fn pointer forces `'a` to equal the
        // lifetime of `&local`, which is the inner block. The implicit
        // reborrow shortens the runtime borrow from `'static` to `'a`.
        fn through<'a>(rt: &'a LeanRuntime, _anchor: &'a ()) -> LeanHost<'a> {
            LeanHost::from_lake_project(rt, "/tmp/nowhere").expect("ignored")
        }

        host = through(runtime, &local);
    }
    // `host` is borrowing `local`, which dropped at the end of the
    // inner block. The next line tries to keep `host` alive in the
    // outer scope — the borrow checker must reject the assignment
    // above.
    let _escape = host;
}
