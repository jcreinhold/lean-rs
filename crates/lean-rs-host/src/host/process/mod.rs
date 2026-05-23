//! Info-tree projection capability on [`crate::LeanSession`].
//!
//! Two sibling entry points project a Lean source into the FFI-safe
//! [`ProcessedFile`] structure:
//!
//! - [`crate::LeanSession::process_with_info_tree`] takes a body-only
//!   snippet (no `import` header). It drives `IO.processCommands`
//!   from byte 0 with an empty `ModuleParserState` — the right call
//!   for inline scratch buffers and tactic-level snippets.
//! - [`crate::LeanSession::process_module_with_info_tree`] takes a full
//!   Lean source file (header + body). It calls `Lean.Parser.parseHeader`
//!   first, then resumes `IO.processCommands` from the parser state
//!   the header parser produced — the right call for real files
//!   beginning with `import …`. Position coordinates in the returned
//!   projection are in the original file's line/column system with no
//!   Rust-side offset arithmetic.
//!
//! Both shims share the projection machinery (one `Elab.InfoTree`
//! walk that records four arrays of structurally distinct nodes plus
//! the elaborator's diagnostics). The projection is the boundary —
//! raw `InfoTree` never crosses the FFI line, because it carries
//! metavariable contexts and `Lean.Expr` values that cannot be revived
//! outside the elaboration session that produced them.
//!
//! Each node carries an explicit `(start_line, start_column, end_line,
//! end_column)` source range so callers can build cursor-position
//! queries without further Lean calls. [`ProcessedFile::term_at`],
//! [`ProcessedFile::tactic_at`], and [`ProcessedFile::references_of`]
//! cover the three queries downstream cursor tooling needs; new lookup
//! shapes are pure Rust on top of the same projection.
//!
//! Both Lean shims are **optional**: a capability dylib forked without
//! either shim collapses to the corresponding `Unsupported` arm at
//! dispatch time, matching the meta-service degradation pattern.

mod info_tree;
mod outcome;

pub use self::info_tree::{CommandInfoNode, NameRefNode, ProcessedFile, TacticInfoNode, TermInfoNode};
pub use self::outcome::{ProcessFileOutcome, ProcessModuleOutcome};
