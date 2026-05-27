//! Opaque, lifetime-bound handles for core Lean semantic values.
//!
//! [`LeanName`], [`LeanLevel`], [`LeanExpr`], and [`LeanDeclaration`] are
//! receipts for owned Lean values; each wraps a crate-internal owned-
//! object handle so the `'lean` parameter cascades from
//! [`crate::LeanRuntime::init`] and the underlying refcount obligation is
//! honoured automatically. Construction and inspection live on the Lean
//! side: the Rust handle has no public methods to mint, decode, or compare
//! the value it carries—those operations require knowledge of Lean
//! constructor layout, which by charter belongs to Lean code, not to this
//! crate.
//!
//! ## How to use a handle
//!
//! Reach into Lean through [`crate::module::LeanModule::exported_unchecked`]; the
//! handle types already implement the (sealed) [`crate::module::LeanAbi`]
//! trait so they can appear as argument or return types in the typed
//! dispatch:
//!
//! ```ignore
//! // SAFETY: the Lean fixture pins this export as `Unit -> Name`.
//! let mk_name = unsafe {
//!     module.exported_unchecked::<((),), LeanName>("lean_rs_fixture_name_anonymous")
//! }?;
//! let n: LeanName = mk_name.call(())?;
//!
//! // SAFETY: the Lean fixture pins this export as `Name -> String`.
//! let name_to_string = unsafe {
//!     module.exported_unchecked::<(LeanName,), String>("lean_rs_fixture_name_to_string")
//! }?;
//! let s: String = name_to_string.call(n)?;
//! ```
//!
//! ## Display text is diagnostic, not a semantic key
//!
//! Any string a handle yields through a Lean-authored export (`toString`,
//! a pretty-printer, a structured-format helper) is a *diagnostic*. Two
//! values that print the same may not be the same Lean value; two values
//! that print differently may compare equal by Lean's notion of equality
//! at a different reduction depth or with different metadata. Use a
//! Lean-authored equality export (e.g. `Name.beq`, `Expr.beq`) when
//! semantics matter, not string comparison.
//!
//! ## Threading
//!
//! Every handle is `!Send + !Sync`, inherited from the crate-internal
//! owned-object handle it wraps. Worker threads attach to Lean through
//! the crate-internal thread-guard machinery; handles created on one
//! thread stay on that thread.

mod declaration;
mod expr;
mod level;
mod name;

pub use self::declaration::LeanDeclaration;
pub use self::expr::LeanExpr;
pub use self::level::LeanLevel;
pub use self::name::LeanName;

#[cfg(test)]
mod tests;
