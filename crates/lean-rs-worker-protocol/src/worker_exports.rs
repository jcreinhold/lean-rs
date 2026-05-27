//! Hidden closed worker capability export shapes.
//!
//! The worker protocol lets downstream callers choose export names, but not
//! ABI shapes. This module is the shared source of truth for the small set of
//! worker operation signatures that may cross the child process boundary.
//! It is public only because parent, child, and harness live in separate
//! crates; it is not downstream extensibility for arbitrary worker exports.

use lean_toolchain::{
    LeanExportAbiRepr, LeanExportArgAbi, LeanExportOwnership, LeanExportResultConvention, LeanExportReturnAbi,
    LeanExportSignature,
};

/// Worker operation shape used for checked capability dispatch.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkerExportOperation {
    Metadata,
    Doctor,
    JsonCommand,
    StreamingCommand,
    FixtureMul,
    FixturePanic,
}

impl WorkerExportOperation {
    /// Stable diagnostic label for the operation shape.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Metadata => "metadata String -> IO String",
            Self::Doctor => "doctor String -> IO String",
            Self::JsonCommand => "json command String -> IO String",
            Self::StreamingCommand => "streaming command String, USize, USize -> IO UInt8",
            Self::FixtureMul => "fixture multiplication UInt64, UInt64 -> UInt64",
            Self::FixturePanic => "fixture panic UInt8 -> Unit",
        }
    }
}

/// Trusted manifest signature for a metadata export.
#[must_use]
pub fn metadata_signature(symbol: impl Into<String>) -> LeanExportSignature {
    string_io_signature(symbol)
}

/// Trusted manifest signature for a doctor export.
#[must_use]
pub fn doctor_signature(symbol: impl Into<String>) -> LeanExportSignature {
    string_io_signature(symbol)
}

/// Trusted manifest signature for a JSON command export.
#[must_use]
pub fn json_command_signature(symbol: impl Into<String>) -> LeanExportSignature {
    string_io_signature(symbol)
}

/// Trusted manifest signature for a streaming command export.
#[must_use]
pub fn streaming_command_signature(symbol: impl Into<String>) -> LeanExportSignature {
    LeanExportSignature::function(
        symbol,
        vec![
            owned_object_arg(),
            scalar_arg(LeanExportAbiRepr::USize),
            scalar_arg(LeanExportAbiRepr::USize),
        ],
        io_return(LeanExportAbiRepr::LeanObject),
    )
}

/// Trusted manifest signature for the private fixture multiplication export.
#[must_use]
pub fn fixture_mul_signature(symbol: impl Into<String>) -> LeanExportSignature {
    LeanExportSignature::function(
        symbol,
        vec![scalar_arg(LeanExportAbiRepr::U64), scalar_arg(LeanExportAbiRepr::U64)],
        pure_return(LeanExportAbiRepr::U64),
    )
}

/// Trusted manifest signature for the private fixture panic export.
#[must_use]
pub fn fixture_panic_signature(symbol: impl Into<String>) -> LeanExportSignature {
    LeanExportSignature::function(
        symbol,
        vec![scalar_arg(LeanExportAbiRepr::U8)],
        pure_return(LeanExportAbiRepr::LeanObject),
    )
}

fn string_io_signature(symbol: impl Into<String>) -> LeanExportSignature {
    LeanExportSignature::function(
        symbol,
        vec![owned_object_arg()],
        io_return(LeanExportAbiRepr::LeanObject),
    )
}

const fn owned_object_arg() -> LeanExportArgAbi {
    LeanExportArgAbi::new(LeanExportAbiRepr::LeanObject, LeanExportOwnership::Owned)
}

const fn scalar_arg(repr: LeanExportAbiRepr) -> LeanExportArgAbi {
    LeanExportArgAbi::new(repr, LeanExportOwnership::None)
}

const fn pure_return(repr: LeanExportAbiRepr) -> LeanExportReturnAbi {
    LeanExportReturnAbi::new(repr, return_ownership(repr), LeanExportResultConvention::Pure)
}

const fn io_return(repr: LeanExportAbiRepr) -> LeanExportReturnAbi {
    LeanExportReturnAbi::new(repr, return_ownership(repr), LeanExportResultConvention::IoResult)
}

const fn return_ownership(repr: LeanExportAbiRepr) -> LeanExportOwnership {
    match repr {
        LeanExportAbiRepr::LeanObject => LeanExportOwnership::Owned,
        LeanExportAbiRepr::U8
        | LeanExportAbiRepr::U16
        | LeanExportAbiRepr::U32
        | LeanExportAbiRepr::U64
        | LeanExportAbiRepr::USize
        | LeanExportAbiRepr::I8
        | LeanExportAbiRepr::I16
        | LeanExportAbiRepr::I32
        | LeanExportAbiRepr::I64
        | LeanExportAbiRepr::ISize
        | LeanExportAbiRepr::F64 => LeanExportOwnership::None,
    }
}
