//! Closed worker calls from application-attested images and symbol contracts.
use super::{LeanCheckedExportError, LeanExported, LeanIo, TrustedWorkerCapability, WorkerExportContract};
use crate::{LeanError, LeanResult};

/// A policy rejection or a checked lookup failure before worker dispatch.
#[derive(Debug)]
pub enum WorkerBindingFailure {
    /// The symbol and operation pair was outside the application's allowlist.
    Policy(LeanError),
    /// The admitted pair failed native symbol, ABI or kind validation.
    Lookup(LeanCheckedExportError),
}
impl std::fmt::Display for WorkerBindingFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Policy(error) => std::fmt::Display::fmt(error, f),
            Self::Lookup(error) => std::fmt::Display::fmt(error, f),
        }
    }
}
impl std::error::Error for WorkerBindingFailure {}

macro_rules! worker_binding {
    ($handle:ident,$bind:ident,$row:ident, $args:ty,$result:ty) => {
        #[doc=concat!("Cached worker binding for `", stringify!($row), "`.")]
        pub struct $handle<'lean, 'cap>(LeanExported<'lean, 'cap, $args, $result>);
        impl<'lean> TrustedWorkerCapability<'lean> {
            #[doc=concat!("Resolve an exact symbol admitted for `",stringify!($row),"`.")]
            ///
            /// # Errors
            ///
            /// Rejects an unlisted symbol, wrong contract, or failed native lookup.
            #[allow(
                unsafe_code,
                reason = "exact symbol and closed operation checked against actual loaded token"
            )]
            pub fn $bind(&self, symbol: &str) -> Result<$handle<'lean, '_>, WorkerBindingFailure> {
                self.require_contract(symbol, WorkerExportContract::$row)
                    .map_err(WorkerBindingFailure::Policy)?;
                // SAFETY: the application attested the full native contract of this exact symbol and row.
                unsafe { self.capability.exported(symbol) }
                    .map($handle)
                    .map_err(WorkerBindingFailure::Lookup)
            }
        }
    };
}
worker_binding!(
    WorkerStringBinding,
    bind_string_io,
    StringIoString,
    (String,),
    LeanIo<String>
);
worker_binding!(WorkerBinaryBinding, bind_binary, U64Binary, (u64, u64), u64);
worker_binding!(WorkerUnitBinding, bind_unit, U8ToUnit, (u8,), ());

impl WorkerStringBinding<'_, '_> {
    /// Call the admitted String IO export.
    /// # Errors
    /// Propagates a native call or decoding failure.
    #[allow(unsafe_code, reason = "private handle fixes the complete String contract")]
    pub fn call(&self, request: String) -> LeanResult<String> {
        // SAFETY: the exact native contract and both owned strings are fixed at binding.
        unsafe { self.0.call(request) }
    }
}
impl WorkerBinaryBinding<'_, '_> {
    /// Call the admitted binary `UInt64` export.
    /// # Errors
    /// Propagates a native call failure.
    #[allow(
        unsafe_code,
        reason = "private handle fixes both UInt64 arguments and the UInt64 result"
    )]
    pub fn call(&self, left: u64, right: u64) -> LeanResult<u64> {
        // SAFETY: scalar slots have the complete attested UInt64 contract.
        unsafe { self.0.call(left, right) }
    }
}
impl WorkerUnitBinding<'_, '_> {
    /// Call the admitted `UInt8` to `Unit` export.
    /// # Errors
    /// Propagates a native call or decoding failure.
    #[allow(
        unsafe_code,
        reason = "private handle fixes the UInt8 argument and boxed Unit result"
    )]
    pub fn call(&self, value: u8) -> LeanResult<()> {
        // SAFETY: the attested export returns the owned Lean Unit representation.
        unsafe { self.0.call(value) }
    }
}
