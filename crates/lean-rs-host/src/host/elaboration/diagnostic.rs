//! Typed diagnostic surface attached to elaboration / kernel-check
//! failures.
//!
//! Lean's `MessageLog` carries a severity tag, a human-readable message,
//! and an optional source position. The capability shim copies each
//! diagnostic into a `Diagnostic` structure on the Lean side; the impls
//! in this module decode them into Rust [`LeanDiagnostic`] /
//! [`LeanPosition`] values without inspecting Lean's internal
//! representation.
//!
//! The encoding contract (mirrored in `Elaboration.lean`):
//!
//! - `Diagnostic = lean_alloc_ctor(0, 3, 1)` — three object slots
//!   (message, position, fileLabel) and one scalar byte (severity at
//!   offset 0 of the scalar tail).
//! - `DiagnosticPos = lean_alloc_ctor(0, 4, 0)` — four object slots
//!   (line, column, endLine, endColumn) carrying `Nat` and `Option Nat`
//!   values. `Nat` is scalar-tagged for small values (always true for
//!   source positions), so each slot decodes through
//!   [`lean_rs::abi::nat::try_to_u64`].

// SAFETY DOC: every `unsafe { ... }` block in this file carries its own
// `// SAFETY:` comment; the blanket allow keeps the scope minimal per
// `docs/architecture/01-safety-model.md`.
#![allow(unsafe_code)]

use lean_rs_sys::ctor::lean_ctor_get_uint8;
use lean_rs_sys::object::{lean_is_scalar, lean_obj_tag, lean_unbox};

use lean_rs::Obj;
use lean_rs::abi::nat;
use lean_rs::abi::structure::{ctor_tag, take_ctor_objects};
use lean_rs::abi::traits::{TryFromLean, conversion_error};
use lean_rs::error::{LeanResult, bound_message};

/// Severity classification attached to each [`LeanDiagnostic`]. Mirrors
/// Lean's `MessageSeverity` constructors at 4.29.1.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum LeanSeverity {
    /// Informational diagnostic; the operation may still have succeeded.
    Info,
    /// Warning diagnostic; the operation may still have succeeded.
    Warning,
    /// Error diagnostic; the operation did not succeed.
    Error,
}

impl LeanSeverity {
    /// Decode the byte the Lean side wrote for the `severity` scalar field.
    /// `0 = info`, `1 = warning`, `2 = error` per the Lean-side
    /// declaration order of `LeanRsHostShims.Elaboration.Severity`.
    fn from_byte(byte: u8) -> LeanResult<Self> {
        match byte {
            0 => Ok(Self::Info),
            1 => Ok(Self::Warning),
            2 => Ok(Self::Error),
            other => Err(conversion_error(format!(
                "Lean Severity tag {other} is not in {{0=info, 1=warning, 2=error}}"
            ))),
        }
    }
}

/// Source position attached to a Lean-emitted diagnostic.
///
/// `line` and `column` are 1-indexed (Lean convention). `end_line` /
/// `end_column` are present when Lean attached an end position to the
/// diagnostic — parser errors usually carry only a start position, while
/// elaborator and kernel errors carry both.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct LeanPosition {
    line: u32,
    column: u32,
    end_line: Option<u32>,
    end_column: Option<u32>,
}

impl LeanPosition {
    /// 1-indexed line number.
    #[must_use]
    pub fn line(&self) -> u32 {
        self.line
    }

    /// 1-indexed column number.
    #[must_use]
    pub fn column(&self) -> u32 {
        self.column
    }

    /// 1-indexed end line, if Lean attached one.
    #[must_use]
    pub fn end_line(&self) -> Option<u32> {
        self.end_line
    }

    /// 1-indexed end column, if Lean attached one.
    #[must_use]
    pub fn end_column(&self) -> Option<u32> {
        self.end_column
    }
}

impl<'lean> TryFromLean<'lean> for LeanPosition {
    fn try_from_lean(obj: Obj<'lean>) -> LeanResult<Self> {
        let [line_o, column_o, end_line_o, end_column_o] = take_ctor_objects::<4>(obj, 0, "DiagnosticPos")?;
        Ok(Self {
            line: decode_nat_u32(line_o, "DiagnosticPos.line")?,
            column: decode_nat_u32(column_o, "DiagnosticPos.column")?,
            end_line: decode_option_nat_u32(end_line_o, "DiagnosticPos.endLine")?,
            end_column: decode_option_nat_u32(end_column_o, "DiagnosticPos.endColumn")?,
        })
    }
}

/// One Lean-emitted diagnostic: severity tag, bounded message, optional
/// source position, and the file label the elaborator received.
///
/// The `message` is structurally bounded at
/// [`lean_rs::LEAN_ERROR_MESSAGE_LIMIT`]: the decoder truncates on a UTF-8
/// char boundary before storing.
#[derive(Clone, Debug)]
pub struct LeanDiagnostic {
    severity: LeanSeverity,
    message: String,
    position: Option<LeanPosition>,
    file_label: String,
}

impl LeanDiagnostic {
    /// The severity tag Lean attached to this diagnostic.
    #[must_use]
    pub fn severity(&self) -> LeanSeverity {
        self.severity
    }

    /// Lean's diagnostic message, truncated to at most
    /// [`lean_rs::LEAN_ERROR_MESSAGE_LIMIT`] bytes on a UTF-8 char
    /// boundary.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    /// Source position when Lean attached one. Parser-level errors
    /// often carry no position; elaborator and kernel errors do.
    #[must_use]
    pub fn position(&self) -> Option<&LeanPosition> {
        self.position.as_ref()
    }

    /// The file label this diagnostic belongs to. Falls back to the
    /// caller-supplied [`crate::host::elaboration::LeanElabOptions::file_label`]
    /// when Lean's own `MessageLog` did not carry a file name.
    #[must_use]
    pub fn file_label(&self) -> &str {
        &self.file_label
    }

    /// Construct a synthetic error-severity diagnostic without a source
    /// position. Used by the host stack when it must surface a
    /// diagnostic that did not originate in Lean's `MessageLog` — for
    /// example, the `LeanMetaResponse::Unsupported` branch built when a
    /// capability dylib does not export the requested meta service.
    /// The `message` is bounded at [`lean_rs::LEAN_ERROR_MESSAGE_LIMIT`]
    /// on a UTF-8 char boundary, mirroring the Lean-decoded path.
    pub(crate) fn synthetic_error(message: String, file_label: String) -> Self {
        Self {
            severity: LeanSeverity::Error,
            message: bound_message(message),
            position: None,
            file_label,
        }
    }
}

impl<'lean> TryFromLean<'lean> for LeanDiagnostic {
    fn try_from_lean(obj: Obj<'lean>) -> LeanResult<Self> {
        require_diagnostic_ctor(&obj)?;
        let ptr = obj.as_raw_borrowed();
        // SAFETY: ctor shape validated above; `lean_ctor_scalar_cptr`
        // starts immediately past the object slots, so offset 0 reads
        // the first scalar tail byte (the severity tag the Lean-side
        // constructor writes via `lean_ctor_set_uint8(_,
        // sizeof(void*)*num_objs, _)`).
        let severity_byte = unsafe { lean_ctor_get_uint8(ptr, 0) };
        let [msg_o, pos_o, label_o] = take_ctor_objects::<3>(obj, 0, "Diagnostic")?;
        let severity = LeanSeverity::from_byte(severity_byte)?;
        let message = bound_message(String::try_from_lean(msg_o)?);
        let position = Option::<LeanPosition>::try_from_lean(pos_o)?;
        let file_label = String::try_from_lean(label_o)?;
        Ok(Self {
            severity,
            message,
            position,
            file_label,
        })
    }
}

/// Tag-only check; the field-count check happens inside
/// [`take_ctor_objects`] below.
fn require_diagnostic_ctor(obj: &Obj<'_>) -> LeanResult<()> {
    let tag = ctor_tag(obj)?;
    if tag == 0 {
        Ok(())
    } else {
        Err(conversion_error(format!(
            "expected Lean Diagnostic ctor (tag 0), found tag {tag}"
        )))
    }
}

/// Decode a Lean `Nat` slot to a `u32`, refusing values that overflow.
fn decode_nat_u32(obj: Obj<'_>, label: &str) -> LeanResult<u32> {
    let raw = nat::try_to_u64(obj)?;
    u32::try_from(raw).map_err(|_| conversion_error(format!("{label} = {raw} exceeds u32 range")))
}

/// Decode a Lean `Option Nat` slot, refusing wrong tags / out-of-range
/// `Some` payloads. Inlined rather than reusing [`Option<T>`]'s blanket
/// impl because the inner `T = Nat` does not match the polymorphic-boxed
/// `u32::try_from_lean` ABI (Nat is scalar-tagged, `UInt32` in
/// polymorphic position is ctor-boxed).
fn decode_option_nat_u32(obj: Obj<'_>, label: &str) -> LeanResult<Option<u32>> {
    let ptr = obj.as_raw_borrowed();
    // SAFETY: pure pointer-bit math.
    if unsafe { lean_is_scalar(ptr) } {
        // SAFETY: scalar branch verified above.
        let payload = unsafe { lean_unbox(ptr) };
        return match payload {
            0 => Ok(None),
            other => Err(conversion_error(format!(
                "{label}: expected Option.none (scalar tag 0), found scalar payload {other}"
            ))),
        };
    }
    // SAFETY: non-scalar branch; tag read on the owned object header.
    let tag = unsafe { lean_obj_tag(ptr) };
    if tag != 1 {
        return Err(conversion_error(format!(
            "{label}: expected Option.some ctor (tag 1), found heap tag {tag}"
        )));
    }
    let [inner] = take_ctor_objects::<1>(obj, 1, "Option Nat")?;
    Ok(Some(decode_nat_u32(inner, label)?))
}
