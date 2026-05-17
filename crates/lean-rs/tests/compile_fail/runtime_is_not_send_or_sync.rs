//! `LeanRuntime`, `LeanSession`, and the semantic handles all carry a
//! `PhantomData<*mut ()>` (directly or transitively via the runtime
//! borrow), so every one of these `require_send` / `require_sync` calls
//! must fail. The `.stderr` snapshot pins the rejection messages so a
//! regression — e.g., an accidental `impl Send` somewhere in the
//! handle chain — gets caught at CI time.

use lean_rs::{LeanExpr, LeanRuntime, LeanSession};

fn require_send<T: Send>() {}
fn require_sync<T: Sync>() {}

fn main() {
    require_send::<LeanRuntime>();
    require_sync::<LeanRuntime>();
    require_send::<LeanSession<'static, 'static>>();
    require_sync::<LeanSession<'static, 'static>>();
    require_send::<LeanExpr<'static>>();
    require_sync::<LeanExpr<'static>>();
}
