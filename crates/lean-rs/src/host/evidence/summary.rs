//! [`ProofSummary`] — Lean-authored display + identifier projection of
//! a [`crate::LeanEvidence`] handle.
//!
//! The summary owns only bounded `String` fields (declared name, kind
//! string, pretty-printed type signature). It carries no `'lean`
//! lifetime because no `Obj<'lean>` survives the decode: the Lean
//! shim materialises the three strings against the session
//! environment before returning, so the Rust value is independent of
//! the runtime that produced it and can be stored, cloned, or sent
//! across thread boundaries.
//!
//! Strings on `ProofSummary` are **display text for diagnostics and
//! storage**. They are not semantic keys; comparing two summaries by
//! field equality does not imply equality of the underlying Lean
//! declarations. **Evidence handles and the summaries derived from
//! them are not proof certificates outside the Lean session that
//! produced or validated them.**

use crate::abi::structure::take_ctor_objects;
use crate::abi::traits::TryFromLean;
use crate::error::LeanResult;
use crate::runtime::obj::Obj;

/// Soft byte cap the Lean-side summariser applies to each `ProofSummary` field.
///
/// Mirrors `proofSummaryByteLimit` in
/// `fixtures/lean/LeanRsFixture/Elaboration.lean`. Bounded so the Rust side
/// can pre-size buffers or budget log lines without an extra FFI round-trip.
pub const LEAN_PROOF_SUMMARY_BYTE_LIMIT: usize = 4 * 1024;

/// Bounded display projection of a kernel-checked Lean declaration.
///
/// Produced by [`crate::LeanSession::summarize_evidence`]. Owns only
/// `String`s, so it has no `'lean` parameter and is freely cloneable
/// or stashable. Every field is bounded by
/// [`LEAN_PROOF_SUMMARY_BYTE_LIMIT`] on the Lean side (UTF-8-safe
/// truncation at a `Char` boundary).
///
/// All fields are diagnostic only. Comparing two summaries by field
/// equality does **not** imply equality of the underlying Lean
/// declarations; route semantic comparisons through a Lean-authored
/// equality export.
#[derive(Clone, Debug)]
pub struct ProofSummary {
    declaration_name: String,
    kind: String,
    type_signature: String,
}

impl ProofSummary {
    /// The declared name rendered as a dotted path (e.g. `"Nat.add"`).
    /// Diagnostic only — multiple distinct `Lean.Name`s can render to
    /// the same dotted string.
    #[must_use]
    pub fn declaration_name(&self) -> &str {
        &self.declaration_name
    }

    /// A human-readable kind tag. The Lean shim emits one of
    /// `"theorem"`, `"definition"`, `"axiom"`, `"opaque"`, or
    /// `"unsupported"` for declaration kinds the
    /// [`crate::LeanSession::kernel_check`] classifier does not
    /// currently produce.
    #[must_use]
    pub fn kind(&self) -> &str {
        &self.kind
    }

    /// Pretty-printed type expression. Format is the default Lean
    /// `ToString Expr` rendering (deterministic, parseable, not
    /// pretty in the elaborator-aware sense). Truncated on a
    /// character boundary at [`LEAN_PROOF_SUMMARY_BYTE_LIMIT`].
    #[must_use]
    pub fn type_signature(&self) -> &str {
        &self.type_signature
    }
}

impl<'lean> TryFromLean<'lean> for ProofSummary {
    /// Decode the three-field Lean `ProofSummary` structure.
    ///
    /// The Lean shim returns `lean_alloc_ctor(0, 3, 0)`: three object
    /// slots (`declarationName`, `kind`, `typeSignature`), no scalar
    /// tail. The Rust decode is the same `take_ctor_objects::<3>`
    /// pattern used by [`crate::host::elaboration::LeanElabFailure`]
    /// in `host/elaboration/failure.rs`.
    fn try_from_lean(obj: Obj<'lean>) -> LeanResult<Self> {
        let [name_obj, kind_obj, type_obj] = take_ctor_objects::<3>(obj, 0, "ProofSummary")?;
        Ok(Self {
            declaration_name: String::try_from_lean(name_obj)?,
            kind: String::try_from_lean(kind_obj)?,
            type_signature: String::try_from_lean(type_obj)?,
        })
    }
}
