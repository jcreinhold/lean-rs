//! Typed outcomes for the two info-tree projection shims on
//! [`crate::LeanSession`].
//!
//! - [`ProcessFileOutcome`] is what
//!   [`crate::LeanSession::process_with_info_tree`] returns: the
//!   body-only projection plus the `Unsupported` degradation arm.
//! - [`ProcessModuleOutcome`] is what
//!   [`crate::LeanSession::process_module_with_info_tree`] returns: three
//!   real arms (`Ok`, `MissingImports`, `HeaderParseFailed`) plus the
//!   same `Unsupported` degradation. The arms distinguish a cleanly
//!   parsed header whose body elaborated, a cleanly parsed header
//!   whose imports the session's open env does not have (soft
//!   failure — the body still runs against the env), and a header
//!   that did not parse at all (the body is never elaborated).
//!
//! Heartbeat exhaustion during a single command's elaboration surfaces
//! as an error-severity entry in `ProcessedFile::diagnostics`, not a
//! separate wire arm — `IO.processCommands` catches per-command
//! exceptions and attaches them to the message log on both shims.

use lean_rs::Obj;
use lean_rs::abi::structure::{ctor_tag, take_ctor_objects};
use lean_rs::abi::traits::{TryFromLean, conversion_error};
use lean_rs::error::LeanResult;

use crate::host::elaboration::LeanElabFailure;
use crate::host::process::info_tree::ProcessedFile;

/// Outcome of [`crate::LeanSession::process_with_info_tree`].
///
/// `#[non_exhaustive]` so future capability refinements can extend the
/// taxonomy without breaking exhaustive matches.
#[derive(Debug)]
#[non_exhaustive]
pub enum ProcessFileOutcome {
    /// The elaborator ran and produced an `Elab.InfoTree` projection.
    /// `ProcessedFile::diagnostics` carries every error-severity entry
    /// the elaborator emitted; callers that need to distinguish
    /// heartbeat exhaustion from other failures should match on the
    /// diagnostics there.
    Processed(ProcessedFile),
    /// The capability dylib does not export the
    /// `lean_rs_host_process_with_info_tree` shim. No FFI call was
    /// made; callers can fall back or degrade as appropriate.
    Unsupported,
}

/// Outcome of [`crate::LeanSession::process_module_with_info_tree`].
///
/// The Lean shim parses the header first (via `Lean.Parser.parseHeader`)
/// and only runs `IO.processCommands` if the header parsed cleanly.
/// `Ok` / `MissingImports` therefore both carry a populated
/// [`ProcessedFile`] (its `diagnostics` field captures any elaboration
/// failure of the body); `HeaderParseFailed` short-circuits with just
/// the parser's diagnostics.
///
/// `#[non_exhaustive]` so future capability refinements can extend the
/// taxonomy without breaking exhaustive matches.
#[derive(Debug)]
#[non_exhaustive]
pub enum ProcessModuleOutcome {
    /// Header parsed; every parsed import is present in the session's
    /// open env; the body was processed. `imports` lists the
    /// user-written modules (Lean's auto-inserted `Init` is filtered
    /// out by the shim).
    Ok {
        /// Body projection. `file.diagnostics` still records any
        /// per-command elaboration errors the body produced.
        file: ProcessedFile,
        /// User-written imports from the file's header.
        imports: Vec<String>,
    },
    /// Header parsed but some imports name modules the session's open
    /// env does not have. The body was still processed against the
    /// available env — `file` is populated and the partial projection
    /// is useful diagnostic data. Callers typically surface `missing`
    /// as a warning.
    MissingImports {
        /// Body projection (partial if some declarations depended on
        /// the missing modules).
        file: ProcessedFile,
        /// User-written imports from the file's header.
        imports: Vec<String>,
        /// Subset of `imports` not present in the session's open env.
        missing: Vec<String>,
    },
    /// `Lean.Parser.parseHeader` reported error-severity messages.
    /// `IO.processCommands` was not invoked; only the header
    /// diagnostics are returned.
    HeaderParseFailed {
        /// Header-parser diagnostics, with the same byte-budget
        /// semantics as
        /// [`crate::host::evidence::LeanKernelOutcome::Rejected`].
        diagnostics: LeanElabFailure,
    },
    /// The capability dylib does not export the
    /// `lean_rs_host_process_module_with_info_tree` shim. No FFI call was
    /// made.
    Unsupported,
}

impl<'lean> TryFromLean<'lean> for ProcessModuleOutcome {
    fn try_from_lean(obj: Obj<'lean>) -> LeanResult<Self> {
        // The Lean inductive has three constructors:
        //   tag 0 = .ok               (file, imports)
        //   tag 1 = .missingImports   (file, imports, missing)
        //   tag 2 = .headerParseFailed(diagnostics)
        // `Unsupported` is synthesised on the Rust side and never
        // crosses the FFI, mirroring the existing pattern on
        // `ProcessFileOutcome`.
        let tag = ctor_tag(&obj)?;
        match tag {
            0 => {
                let [file, imports] = take_ctor_objects::<2>(obj, 0, "ProcessModuleOutcome::ok")?;
                Ok(Self::Ok {
                    file: ProcessedFile::try_from_lean(file)?,
                    imports: Vec::<String>::try_from_lean(imports)?,
                })
            }
            1 => {
                let [file, imports, missing] = take_ctor_objects::<3>(obj, 1, "ProcessModuleOutcome::missingImports")?;
                Ok(Self::MissingImports {
                    file: ProcessedFile::try_from_lean(file)?,
                    imports: Vec::<String>::try_from_lean(imports)?,
                    missing: Vec::<String>::try_from_lean(missing)?,
                })
            }
            2 => {
                let [diagnostics] = take_ctor_objects::<1>(obj, 2, "ProcessModuleOutcome::headerParseFailed")?;
                Ok(Self::HeaderParseFailed {
                    diagnostics: LeanElabFailure::try_from_lean(diagnostics)?,
                })
            }
            other => Err(conversion_error(format!(
                "expected Lean ProcessModuleOutcome ctor (tag 0..=2), found tag {other}"
            ))),
        }
    }
}
