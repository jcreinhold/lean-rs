//! [`LeanElabFailure`] — the typed-diagnostic payload returned by the
//! inner `Result` of [`crate::LeanSession::elaborate`] and by the
//! non-`Checked` variants of [`crate::host::evidence::LeanKernelOutcome`].
//!
//! The Lean side returns a structure carrying an `Array Diagnostic` and
//! a `Truncation` tag indicating whether the diagnostic byte budget was
//! hit. The encoding is `lean_alloc_ctor(0, 1, 1)` — one object slot
//! (the diagnostics array) and one scalar byte (the truncation tag at
//! scalar tail offset 0).

// SAFETY DOC: the single `unsafe { ... }` block in this file carries
// its own `// SAFETY:` comment; the blanket allow keeps the scope
// minimal per `docs/architecture/01-safety-model.md`.
#![allow(unsafe_code)]

use core::fmt;

use lean_rs_sys::ctor::lean_ctor_get_uint8;

use crate::host::elaboration::diagnostic::{LeanDiagnostic, LeanSeverity};
use lean_rs::Obj;
use lean_rs::abi::structure::{ctor_tag, take_ctor_objects};
use lean_rs::abi::traits::{TryFromLean, conversion_error};
use lean_rs::error::{LeanDiagnosticCode, LeanResult};

/// Failure payload carrying typed diagnostics from a Lean elaboration
/// or kernel-check call.
///
/// Returned in the inner `Result` of [`crate::LeanSession::elaborate`]
/// and inside the non-`Checked` variants of
/// [`crate::host::evidence::LeanKernelOutcome`]. The collection is
/// bounded: the Lean side stops adding diagnostics once the running
/// byte sum would exceed
/// [`crate::host::elaboration::LeanElabOptions::diagnostic_byte_limit`]
/// and sets [`Self::truncated`] to `true` so callers can detect the
/// shortened report.
#[derive(Clone, Debug)]
pub struct LeanElabFailure {
    diagnostics: Vec<LeanDiagnostic>,
    truncated: bool,
}

impl LeanElabFailure {
    /// The diagnostics Lean emitted, in the order it produced them.
    /// Empty only when the failure path itself produced no diagnostics
    /// (in which case [`Self::truncated`] is also `false`).
    #[must_use]
    pub fn diagnostics(&self) -> &[LeanDiagnostic] {
        &self.diagnostics
    }

    /// Whether the Lean side hit the configured diagnostic byte budget
    /// (`LeanElabOptions::diagnostic_byte_limit`) and stopped collecting.
    /// When `true`, the diagnostics returned are a prefix of what Lean
    /// would have produced under an unbounded budget.
    #[must_use]
    pub fn truncated(&self) -> bool {
        self.truncated
    }

    /// Project to the stable [`LeanDiagnosticCode`] taxonomy.
    ///
    /// Always [`LeanDiagnosticCode::Elaboration`] — the variant is here
    /// for uniformity with [`lean_rs::LeanError::code`] and
    /// [`LeanMetaResponse::code`](crate::host::meta::LeanMetaResponse::code).
    #[must_use]
    pub const fn code(&self) -> LeanDiagnosticCode {
        LeanDiagnosticCode::Elaboration
    }

    /// Construct a one-message failure carrying a host-synthesised
    /// error diagnostic (no Lean source). Used by
    /// [`crate::LeanSession::run_meta`] when a capability dylib does
    /// not export the requested meta service — there is no Lean shim
    /// to produce diagnostics, so the host stack builds one itself.
    /// `truncated` is always `false` for synthesised failures.
    pub(crate) fn synthetic(message: String, file_label: String) -> Self {
        Self {
            diagnostics: vec![LeanDiagnostic::synthetic_error(message, file_label)],
            truncated: false,
        }
    }
}

impl fmt::Display for LeanElabFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Prefer the first error-severity diagnostic; fall back to the
        // first of any severity. Either way, the message is already
        // bounded at LEAN_ERROR_MESSAGE_LIMIT per LeanDiagnostic decode.
        if let Some(first_error) = self.diagnostics.iter().find(|d| d.severity() == LeanSeverity::Error) {
            return f.write_str(first_error.message());
        }
        if let Some(first) = self.diagnostics.first() {
            return f.write_str(first.message());
        }
        f.write_str("elaboration failed (no diagnostics)")
    }
}

impl std::error::Error for LeanElabFailure {}

impl<'lean> TryFromLean<'lean> for LeanElabFailure {
    fn try_from_lean(obj: Obj<'lean>) -> LeanResult<Self> {
        // ElabFailure is `lean_alloc_ctor(0, 1, 1)` — one object slot
        // (diagnostics) and one scalar byte (truncated). Read the
        // scalar before consuming through `take_ctor_objects`.
        let tag = ctor_tag(&obj)?;
        if tag != 0 {
            return Err(conversion_error(format!(
                "expected Lean ElabFailure ctor (tag 0), found tag {tag}"
            )));
        }
        let ptr = obj.as_raw_borrowed();
        // SAFETY: ctor validated above; the first scalar-tail byte holds
        // the Truncation tag (0 = complete, 1 = truncated).
        let truncated_byte = unsafe { lean_ctor_get_uint8(ptr, 0) };
        let truncated = match truncated_byte {
            0 => false,
            1 => true,
            other => {
                return Err(conversion_error(format!(
                    "Lean Truncation tag {other} is not in {{0=complete, 1=truncated}}"
                )));
            }
        };
        let [diagnostics_o] = take_ctor_objects::<1>(obj, 0, "ElabFailure")?;
        let diagnostics = Vec::<LeanDiagnostic>::try_from_lean(diagnostics_o)?;
        Ok(Self { diagnostics, truncated })
    }
}
