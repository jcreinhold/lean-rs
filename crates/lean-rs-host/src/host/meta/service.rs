//! [`LeanMetaService`] — sealed descriptor for a registered bounded
//! `MetaM` service.
//!
//! Each pinned service ties together:
//!
//! - the Lean-side `@[export]` symbol name the dispatcher resolves;
//! - the list of `.olean` modules that must be imported into the
//!   session before the service can run;
//! - the typed request / response shape (encoded as `PhantomData<fn(Req)
//!   -> Resp>`) the [`crate::LeanSession::run_meta`] generic
//!   parameters must agree with.
//!
//! The struct has no public constructor — the only `LeanMetaService`
//! values downstream code can name are returned by the four free
//! functions in this module, which form the closed registry. Adding a
//! new service requires landing a Lean `@[export]` *and* a Rust-side
//! constructor here.

use core::marker::PhantomData;

use lean_rs::LeanExpr;

use super::LeanMetaTransparency;

/// Sealed descriptor for one registered `MetaM` service.
///
/// `Req` is the Rust-side type the call site supplies; `Resp` is the
/// type the typed payload decodes into on the `Ok` branch. The phantom
/// `fn(Req) -> Resp` keeps the marker invariant in both parameters
/// (matching how Lake's emitted signature treats them).
#[derive(Clone, Copy)]
pub struct LeanMetaService<Req, Resp> {
    name: &'static str,
    required_imports: &'static [&'static str],
    _marker: PhantomData<fn(Req) -> Resp>,
}

impl<Req, Resp> LeanMetaService<Req, Resp> {
    /// Crate-internal constructor. The pinned service free fns below
    /// are the only callers.
    pub(crate) const fn new(name: &'static str, required_imports: &'static [&'static str]) -> Self {
        Self {
            name,
            required_imports,
            _marker: PhantomData,
        }
    }

    /// The Lean-side C-symbol name the dispatcher resolves.
    #[must_use]
    pub fn name(&self) -> &'static str {
        self.name
    }

    /// The `.olean` module names that must appear in the session's
    /// import list before this service can run. Callers can either
    /// thread the list into [`crate::LeanCapabilities::session`] or
    /// import the parent `LeanRsFixture` roll-up module, which
    /// transitively imports every fixture submodule.
    #[must_use]
    pub fn required_imports(&self) -> &'static [&'static str] {
        self.required_imports
    }
}

impl<Req, Resp> core::fmt::Debug for LeanMetaService<Req, Resp> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("LeanMetaService")
            .field("name", &self.name)
            .field("required_imports", &self.required_imports)
            .finish()
    }
}

// -- The four pinned services ------------------------------------------
//
// Each constructor returns the typed descriptor `'lean`-parameterised
// over the request/response payload. The Lean-side symbol names and
// import strings are pinned here; the `LeanMetaService` shape is
// otherwise sealed.

const REQUIRED_IMPORTS: &[&str] = &["LeanRsHostShims.Meta"];

/// Register the `Meta.inferType` service.
///
/// Calling [`crate::LeanSession::run_meta`] with the returned
/// descriptor and a [`LeanExpr`] returns a typed
/// [`super::LeanMetaResponse`] carrying the inferred type expression on
/// `Ok`.
#[must_use]
pub fn infer_type<'lean>() -> LeanMetaService<LeanExpr<'lean>, LeanExpr<'lean>> {
    LeanMetaService::new("lean_rs_host_meta_infer_type", REQUIRED_IMPORTS)
}

/// Register the `Meta.whnf` service.
///
/// Returns the weak-head normal form of the supplied expression under
/// the bundle's reducibility setting.
#[must_use]
pub fn whnf<'lean>() -> LeanMetaService<LeanExpr<'lean>, LeanExpr<'lean>> {
    LeanMetaService::new("lean_rs_host_meta_whnf", REQUIRED_IMPORTS)
}

/// Register the diagnostic heartbeat-burn service.
///
/// The Lean-side action loops on `Core.checkMaxHeartbeats`; any nonzero
/// heartbeat budget below the loop bound trips
/// `Lean.Exception.isMaxHeartbeat` and surfaces as
/// `LeanMetaResponse::TimeoutOrHeartbeat`. The supplied `LeanExpr` is
/// ignored — the request shape is uniform with the inference / whnf
/// services so callers do not need a separate "no input" descriptor.
#[must_use]
pub fn heartbeat_burn<'lean>() -> LeanMetaService<LeanExpr<'lean>, LeanExpr<'lean>> {
    LeanMetaService::new("lean_rs_host_meta_heartbeat_burn", REQUIRED_IMPORTS)
}

/// Register the `Meta.isDefEq` service.
///
/// The request is the Lean product `(lhs, rhs, transparency)`. The
/// response remains a [`super::LeanMetaResponse<bool>`], so heartbeat
/// exhaustion and Lean exceptions stay distinct from a successful
/// `false`.
#[must_use]
pub fn is_def_eq<'lean>() -> LeanMetaService<(LeanExpr<'lean>, LeanExpr<'lean>, LeanMetaTransparency), bool> {
    LeanMetaService::new("lean_rs_host_meta_is_def_eq", REQUIRED_IMPORTS)
}

/// Register the `Lean.PrettyPrinter.ppExpr` service.
///
/// Returns the pretty-printed string form of the supplied expression —
/// the form a Lean user reads. `MetaM`-bounded, so a deeply nested
/// term under a tight heartbeat budget surfaces as
/// [`super::LeanMetaResponse::TimeoutOrHeartbeat`]. For a cheap,
/// deterministic, ugly alternative that pays no `MetaM` cost, use
/// [`crate::LeanSession::expr_to_string_raw`].
///
/// Optional symbol: capability dylibs that predate this shim still
/// load, and [`crate::LeanSession::run_meta`] returns
/// [`super::LeanMetaResponse::Unsupported`] for the call. Callers that
/// want graceful degradation should fall through to
/// [`crate::LeanSession::expr_to_string_raw`] on the `Unsupported`
/// branch.
#[must_use]
pub fn pp_expr<'lean>() -> LeanMetaService<LeanExpr<'lean>, String> {
    LeanMetaService::new("lean_rs_host_meta_pp_expr", REQUIRED_IMPORTS)
}
