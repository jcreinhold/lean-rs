//! Bounded module-query projection capability on [`crate::LeanSession`].
//!
//! [`crate::LeanSession::process_module_query`] takes a full Lean source
//! file plus a [`ModuleQuery`]. Lean parses the header, elaborates the
//! body, performs the requested cursor/reference/diagnostic projection,
//! and returns only that bounded result. Raw whole-file `InfoTree`
//! projections do not cross the FFI boundary.

mod query;

pub use self::query::{
    GoalAtResult, ModuleQuery, ModuleQueryOutcome, ModuleQueryResult, ModuleSourceSpan, NameRefNode, ReferencesResult,
    RenderedInfo, TypeAtResult,
};
