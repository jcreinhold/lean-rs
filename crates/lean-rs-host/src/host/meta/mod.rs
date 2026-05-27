//! Bounded `MetaM` / `CoreM` capability surface.
//!
//! Rust can invoke a **closed registry** of Lean-authored meta services
//! through [`crate::LeanSession::run_meta`]. Each service is a sealed
//! [`LeanMetaService`] descriptor pinning a name, the `.olean` modules
//! required in the session, and a typed `(Req, Resp)` shape. Four
//! services are pinned:
//!
//! - [`infer_type`] — runs `Meta.inferType` on a supplied `Expr`.
//! - [`whnf`] — runs `Meta.whnf` on a supplied `Expr` under the bundle's
//!   reducibility setting.
//! - [`heartbeat_burn`] — a diagnostic loop that consumes a heartbeat
//!   per step, exercising the heartbeat-exhaustion classification path.
//! - [`is_def_eq`] — runs `Meta.isDefEq` on two supplied expressions
//!   under a request-supplied transparency setting.
//! - [`pp_expr`] — runs `Lean.PrettyPrinter.ppExpr` on a supplied `Expr`
//!   and returns the rendered string. Slow relative to the raw
//!   [`crate::LeanSession::expr_to_string_raw`] projection but produces
//!   the form a Lean user reads.
//!
//! Rust cannot assemble arbitrary `MetaM` programs: the only services
//! that exist are the ones with a `LeanMetaService` constructor in this
//! crate and a matching `@[export]` symbol on the Lean side
//! (`lean_rs_host_meta_*`). New services require landing both pieces
//! together.
//!
//! ## Bounded options
//!
//! [`LeanMetaOptions`] mirrors [`crate::LeanElabOptions`]: saturating
//! setters for heartbeats and diagnostic byte budget plus a
//! MetaM-specific [`LeanMetaTransparency`] knob. The heartbeat and
//! diagnostic-byte ceilings reuse the existing
//! `LEAN_HEARTBEAT_LIMIT_*` / `LEAN_DIAGNOSTIC_BYTE_LIMIT_*` constants
//! re-exported at the crate root.
//!
//! ## Status classification
//!
//! Every call returns a [`LeanMetaResponse<Resp>`] tagged by
//! [`MetaCallStatus`]:
//!
//! | Status                | Source                                                        |
//! | --------------------- | ------------------------------------------------------------- |
//! | `Ok`                  | `MetaM` action returned a payload                               |
//! | `Failed`              | `MetaM` raised a non-heartbeat Lean exception                   |
//! | `TimeoutOrHeartbeat`  | `Exception.isMaxHeartbeat` matched on the caught exception    |
//! | `Unsupported`         | Lean shim classified the request out-of-domain, **or** the loaded capability does not export the service's symbol (the Rust dispatcher synthesises this branch from a missing optional binding) |
//!
//! The outer [`lean_rs::LeanResult`] still carries true host-stack
//! failures (a Lean shim *itself* raising through `IO`, or a malformed
//! return value); the four-way classification lives in the inner
//! `LeanMetaResponse`.

mod options;
mod response;
mod service;

pub use self::options::{LeanMetaOptions, LeanMetaTransparency};
pub use self::response::{LeanMetaResponse, MetaCallStatus};
pub use self::service::{LeanMetaService, heartbeat_burn, infer_type, is_def_eq, pp_expr, whnf};
