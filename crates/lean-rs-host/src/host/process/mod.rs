//! Info-tree projection capability on [`crate::LeanSession`].
//!
//! [`crate::LeanSession::process_with_info_tree`] drives Lean's
//! `IO.processCommands` pipeline with info collection enabled, then
//! projects the resulting `Elab.InfoTree` forest into the FFI-safe
//! [`ProcessedFile`] structure: four arrays (commands, terms, tactics,
//! name references) plus the diagnostics the elaborator emitted. The
//! projection is the boundary — raw `InfoTree` never crosses the FFI
//! line, because it carries metavariable contexts and `Lean.Expr`
//! values that cannot be revived outside the elaboration session that
//! produced them.
//!
//! Each node carries an explicit `(start_line, start_column, end_line,
//! end_column)` source range so callers can build cursor-position
//! queries without further Lean calls. [`ProcessedFile::term_at`],
//! [`ProcessedFile::tactic_at`], and [`ProcessedFile::references_of`]
//! cover the three queries downstream cursor tooling needs; new lookup
//! shapes are pure Rust on top of the same projection.
//!
//! The Lean shim is **optional**: a capability dylib forked without
//! this shim collapses to [`ProcessFileOutcome::Unsupported`] at
//! dispatch time, matching the meta-service degradation pattern.

mod info_tree;
mod outcome;

pub use self::info_tree::{CommandInfoNode, NameRefNode, ProcessedFile, TacticInfoNode, TermInfoNode};
pub use self::outcome::ProcessFileOutcome;
