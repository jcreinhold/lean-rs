//! L1 curated-surface gate. Compile-only: asserts every item the
//! `lean-rs` crate root re-exports stays at `lean_rs::*` and keeps its
//! shape. Imports use only `use lean_rs::{...}` — no module paths, no
//! sibling-crate names. If the file fails to compile because an import
//! resolves with a different shape (or fails to resolve), the curated L1
//! surface and `docs/api-review/lean-rs-public.txt` are out of sync and
//! one must be brought into agreement before widening the imports here.
//!
//! There is no FFI in this test by design: `lean-rs` has no Lean-side
//! fixture of its own (the L1 happy-path lives in the external
//! `lean-rs-downstream` proof at the sibling repository); the structural
//! compile-time gate is the in-tree CI signal. The L2 sibling crate has
//! its own `tests/curated_surface.rs` that drives the host-stack happy
//! path against the workspace fixture.

use lean_rs::{
    CapturedEvent, DIAGNOSTIC_CAPTURE_DEFAULT_CAPACITY, DecodeCallResult, DiagnosticCapture, HostFailure, HostStage,
    LEAN_ERROR_MESSAGE_LIMIT, LeanAbi, LeanArgs, LeanCallbackFlow, LeanCallbackHandle, LeanCallbackPayload,
    LeanCallbackStatus, LeanDeclaration, LeanDiagnosticCode, LeanError, LeanException, LeanExceptionKind, LeanExported,
    LeanExpr, LeanIo, LeanLevel, LeanLibrary, LeanModule, LeanName, LeanProgressTick, LeanResult, LeanRuntime,
    LeanStringEvent, LeanThreadGuard, Obj, ObjRef, VERSION,
};

#[test]
fn l1_curated_surface_is_reachable_from_crate_root() {
    // Constants resolve as their declared types.
    let _: usize = LEAN_ERROR_MESSAGE_LIMIT;
    let _: usize = DIAGNOSTIC_CAPTURE_DEFAULT_CAPACITY;
    let _: &str = VERSION;

    // The compiler walks every type below as a name-only reachability
    // assertion; the function never runs (asserting `false` keeps the
    // body live for type-checking without smuggling in real FFI work).
    fn _surface_shapes<'lean, 'lib>(
        _runtime: &'lean LeanRuntime,
        _guard: LeanThreadGuard<'lean>,
        _library: LeanLibrary<'lean>,
        _module: LeanModule<'lean, 'lib>,
        _exported: LeanExported<'lean, 'lib, (u64,), u64>,
        _io_marker: LeanIo<u64>,
        _name: LeanName<'lean>,
        _level: LeanLevel<'lean>,
        _expr: LeanExpr<'lean>,
        _decl: LeanDeclaration<'lean>,
        _obj: Obj<'lean>,
        _obj_ref: ObjRef<'lean, '_>,
        _result: LeanResult<()>,
        _err: LeanError,
        _exc: LeanException,
        _host_fail: HostFailure,
        _stage: HostStage,
        _kind: LeanExceptionKind,
        _code: LeanDiagnosticCode,
        _callback_flow: LeanCallbackFlow,
        _callback_handle: LeanCallbackHandle<LeanProgressTick>,
        _string_callback_handle: LeanCallbackHandle<LeanStringEvent>,
        _callback_status: LeanCallbackStatus,
        _progress_tick: LeanProgressTick,
        _string_event: LeanStringEvent,
        _capture: DiagnosticCapture,
        _event: CapturedEvent,
    ) where
        u64: LeanAbi<'lean> + DecodeCallResult<'lean>,
        (u64,): LeanArgs<'lean>,
        LeanProgressTick: LeanCallbackPayload,
        LeanStringEvent: LeanCallbackPayload,
    {
    }
}
