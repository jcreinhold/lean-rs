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
//! infrastructure. Typed exported-function handles cross the public
//! boundary as [`LeanExported`] and [`LeanIo`]; see `exported` for
//! the call shape.

pub(crate) mod capability;
pub(crate) mod exported;
pub(crate) mod initializer;
pub(crate) mod library;
pub(crate) mod loaded;

pub use capability::{LeanBuiltCapability, LeanCapability};
pub use exported::{DecodeCallResult, LeanArgs, LeanExported, LeanIo};
pub use library::LeanLibrary;
pub use loaded::LeanModule;

// `LeanAbi` lives in `crate::abi::traits` but appears in the public
// signature of [`LeanModule::exported`] (as a per-arg bound) and in the
// docstrings for [`LeanExported`]/[`LeanIo`]. Re-export it at the
// `module` boundary so rustdoc resolves intra-crate links and so a
// downstream crate that wants to inspect the bound has a single import
// path. The trait remains sealed; the re-export only widens the doc
// surface, not the impl surface.
pub use crate::abi::traits::LeanAbi;

#[cfg(test)]
mod tests;
