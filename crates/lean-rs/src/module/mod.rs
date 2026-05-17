//! Loading and initializing compiled Lean modules.
//!
//! The public surface of this module is two RAII types:
//!
//! - [`LeanLibrary`] — a Lake-built native shared object opened through
//!   the platform dynamic loader.
//! - [`LeanModule`] — proof that a named Lean module hosted by a
//!   [`LeanLibrary`] has been initialized to `IO.ok(())`.
//!
//! Construction of either type requires a [`crate::LeanRuntime`]
//! borrow, so use-before-init is structurally impossible. The
//! `'lean` lifetime cascade keeps every derived handle bound to the
//! runtime witness; the secondary `'lib` lifetime on [`LeanModule`]
//! keeps initialized modules from outliving the dylib that hosts them.
//!
//! ```ignore
//! let runtime = lean_rs::LeanRuntime::init()?;
//! let library = lean_rs::module::LeanLibrary::open(runtime, "path/to/libfoo.dylib")?;
//! let module  = library.initialize_module("foo_pkg", "Foo.Bar")?;
//! ```
//!
//! Callers do not name raw initializer symbols, choose the Lean
//! `builtin` flag, decode `IO` results, or maintain idempotency state;
//! all of that lives in the `pub(crate) module::initializer`
//! infrastructure. Typed exported-function handles
//! (`LeanExported{N}`) attach to [`LeanModule`] in prompt 12.

pub(crate) mod initializer;
pub(crate) mod library;
pub(crate) mod loaded;

pub use library::LeanLibrary;
pub use loaded::LeanModule;

#[cfg(test)]
mod tests;
