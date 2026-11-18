//! Opaque handle for `Lean.Expr`.
//!
//! [`LeanExpr`] is a receipt for an owned Lean expression value produced
//! on the Lean side. The Rust API is intentionally minimal: it carries
//! the handle through the FFI boundary and nothing else. Construction
//! and inspection—`.bvar`, `.const`, `.app`, `toString`, `==`—live
//! in Lean exports the caller reaches through
//! [`crate::module::LeanModule::exported_unchecked`]. Rust deliberately offers no
//! constructor for `Expr`: rebuilding the Lean term algebra outside the
//! kernel would either be wrong (lacking elaboration / type checking)
//! or duplicate Lean's responsibility.
//!
//! Display text obtained from a Lean export is diagnostic, not a
//! semantic key; use a Lean-authored equality export when semantics
//! matter (see the module docs on [`crate::handle`]).
//!
//! Two render paths cover the cost/quality tradeoff and the host stack
//! exposes both deliberately. `LeanSession::expr_to_string_raw` walks
//! `Expr.toString` directly—cheap, deterministic, ugly—and is the
//! right choice for indexing, logging, and search keys. The optional
//! `lean_rs_host::meta::pp_expr` service runs
//! `Lean.PrettyPrinter.ppExpr` under the standard heartbeat budget; it
//! is the form a Lean user reads, but pays for elaboration context and
//! can time out. There is intentionally no `Display`, `Eq`, or
//! `FromStr` impl on the handle itself: forcing callers through an
//! explicit method keeps the FFI cost and the "diagnostic only"
//! semantics visible at the call site.

use core::fmt;

use lean_rs_sys::lean_object;

use crate::abi::traits::{IntoLean, LeanAbi, LeanBoxedAbi};
use crate::error::LeanResult;
use crate::runtime::LeanRuntime;
use crate::runtime::obj::Obj;

/// Opaque, lifetime-bound handle to a `Lean.Expr`.
///
/// `'lean` cascades from the [`crate::LeanRuntime`] borrow that produced
/// the value, so a handle cannot outlive the runtime. Neither [`Send`]
/// nor [`Sync`]: inherited from the crate-internal owned-object handle
/// the type wraps.
///
/// [`Clone`] bumps the underlying refcount; [`Drop`] releases it. There
/// are no public inherent methods: the handle is a pass-through that
/// reaches Lean-authored operations through
/// [`crate::module::LeanModule::exported_unchecked`].
pub struct LeanExpr<'lean> {
    obj: Obj<'lean>,
}

impl Clone for LeanExpr<'_> {
    fn clone(&self) -> Self {
        Self { obj: self.obj.clone() }
    }
}

impl fmt::Debug for LeanExpr<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Opaque on purpose: the Rust side has no claim on Lean's
        // structural identity. For a diagnostic string, route through a
        // Lean-authored `Expr` pretty-printer export.
        f.debug_struct("LeanExpr").finish_non_exhaustive()
    }
}

#[allow(
    unsafe_code,
    reason = "native conversion requires the caller-established semantic type"
)]
// SAFETY: the private handle stores its native semantic type; encoding transfers its reference and raw decoding requires that type.
unsafe impl<'lean> LeanAbi<'lean> for LeanExpr<'lean> {
    type CRepr = *mut lean_object;

    fn into_c(self, _runtime: &'lean LeanRuntime) -> *mut lean_object {
        self.obj.into_raw()
    }

    unsafe fn from_c(c: *mut lean_object, runtime: &'lean LeanRuntime) -> LeanResult<Self> {
        // SAFETY: `c` is an owned `lean_object*` produced by a Lean
        // export returning `Lean.Expr`; per Lake's `lean_obj_res`
        // contract it carries one reference count. `runtime` witnesses
        // `'lean`.
        #[allow(unsafe_code)]
        let obj = unsafe { Obj::from_owned_raw(runtime, c) };
        Ok(Self { obj })
    }
}

#[allow(
    unsafe_code,
    reason = "native conversion requires the caller-established semantic type"
)]
// SAFETY: private handle construction preserves the semantic type; typed native decoding retains its owned object and lifetime.
unsafe impl<'lean> LeanBoxedAbi<'lean> for LeanExpr<'lean> {
    fn into_boxed(self, _runtime: &'lean LeanRuntime) -> Obj<'lean> {
        self.obj
    }

    unsafe fn from_boxed(obj: Obj<'lean>) -> LeanResult<Self> {
        Ok(Self { obj })
    }
}

impl<'lean> IntoLean<'lean> for LeanExpr<'lean> {
    fn into_lean(self, _runtime: &'lean LeanRuntime) -> Obj<'lean> {
        // The handle already wraps a fully-owned Lean object reference;
        // hand it off unchanged. Required so `Option<LeanExpr<'lean>>`
        // can satisfy [`LeanAbi`] (which is itself bounded on
        // `LeanBoxedAbi`) at the `LeanSession::elaborate`
        // call site that takes an optional expected type.
        self.obj
    }
}
