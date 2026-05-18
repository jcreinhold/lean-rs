//! Every public Lean-derived handle carries a `PhantomData<*mut ()>`
//! (directly or transitively via the `LeanRuntime` borrow), so every
//! `require_send` / `require_sync` call below must fail. The `.stderr`
//! snapshot pins the rejection messages so an accidental `impl Send` or
//! `impl Sync` anywhere in the handle chain gets caught at CI time.
//!
//! Coverage: the full set of public handles per
//! `docs/architecture/04-concurrency.md`.

use lean_rs::{LeanDeclaration, LeanExpr, LeanLevel, LeanName, LeanRuntime, LeanThreadGuard};
use lean_rs_host::{LeanCapabilities, LeanEvidence, LeanHost, LeanSession, PooledSession, SessionPool};

fn require_send<T: Send>() {}
fn require_sync<T: Sync>() {}

fn main() {
    require_send::<LeanRuntime>();
    require_sync::<LeanRuntime>();
    require_send::<LeanThreadGuard<'static>>();
    require_sync::<LeanThreadGuard<'static>>();
    require_send::<LeanHost<'static>>();
    require_sync::<LeanHost<'static>>();
    require_send::<LeanCapabilities<'static, 'static>>();
    require_sync::<LeanCapabilities<'static, 'static>>();
    require_send::<LeanSession<'static, 'static>>();
    require_sync::<LeanSession<'static, 'static>>();
    require_send::<LeanName<'static>>();
    require_sync::<LeanName<'static>>();
    require_send::<LeanLevel<'static>>();
    require_sync::<LeanLevel<'static>>();
    require_send::<LeanExpr<'static>>();
    require_sync::<LeanExpr<'static>>();
    require_send::<LeanDeclaration<'static>>();
    require_sync::<LeanDeclaration<'static>>();
    require_send::<LeanEvidence<'static>>();
    require_sync::<LeanEvidence<'static>>();
    require_send::<SessionPool<'static>>();
    require_sync::<SessionPool<'static>>();
    require_send::<PooledSession<'static, 'static, 'static>>();
    require_sync::<PooledSession<'static, 'static, 'static>>();
}
