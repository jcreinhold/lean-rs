//! Typed Lean module initializer symbol names and invocation.
//!
//! The `pub(crate)` machinery that translates a `(package, module)` pair
//! into the C symbol Lake exports for a module initializer, validates the
//! component names so unvalidated strings cannot reach `dlsym`, and runs
//! the initializer under a panic boundary with the correct `builtin`
//! policy. Everything here is `pub(crate)`; callers of `lean-rs` reach
//! initialization through [`crate::module::LeanLibrary::initialize_module`]
//! and never name the raw symbol or the `builtin` flag.
//!
//! ## Mangling
//!
//! Lake mangles each component of `<package>.<module-path>` by replacing
//! every literal `_` with `__`, joins the components with single `_`, and
//! prefixes with `initialize_`. The Lake-emitted C in
//! `fixtures/lean/.lake/build/ir/LeanRsFixture.c` is the anchoring
//! example: package `lean_rs_fixture`, module `LeanRsFixture` gives the
//! symbol `initialize_lean__rs__fixture_LeanRsFixture`.
//!
//! ## Idempotency
//!
//! The Lean-emitted C body of every initializer begins with
//! `if (_G_initialized) return lean_io_result_mk_ok(lean_box(0));`. The
//! second and later calls return `IO.ok(())` in a handful of instructions
//! without re-running the cascading sub-initializers. `lean-rs` therefore
//! does **not** maintain a Rust-side "already initialized" cache;
//! delegating to the Lean static keeps the invariant in exactly one place
//! and removes interior mutability from `LeanLibrary`.

// SAFETY DOC: every `unsafe { ... }` block in this file carries its own
// `// SAFETY:` comment. The blanket allow exists because this is the
// single `pub(crate)` site that runs Lean-generated module initializers
// — a per-file opt-out per `docs/architecture/01-safety-model.md`.
#![allow(unsafe_code)]

use std::ffi::CString;
use std::panic::{self, AssertUnwindSafe};
use std::ptr::NonNull;

use lean_rs_sys::lean_object;

#[cfg(doc)]
use crate::error::HostStage;
use crate::error::io::decode_io;
use crate::error::{LeanError, LeanResult};
use crate::runtime::LeanRuntime;
use crate::runtime::obj::Obj;

/// The Lake-mangled C symbol name for a module's initializer, plus a
/// human-readable display form for diagnostics.
///
/// Constructed only by [`InitializerName::from_lake_names`], which is the
/// single place that validates input and applies the Lake mangling rule.
/// External code never sees raw symbol bytes; consumers inside the crate
/// reach them through [`symbol_bytes`](Self::symbol_bytes) for `dlsym`.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) struct InitializerName {
    /// C-string-ready bytes (with the trailing NUL) for libloading's
    /// symbol lookup.
    symbol: CString,
    /// Human-readable form, e.g. `lean_rs_fixture::LeanRsFixture.Scalars`.
    display: String,
}

impl InitializerName {
    /// Validate and mangle a `(package, module)` pair into the C symbol
    /// Lake exports for that module's initializer.
    ///
    /// `package` is a single Lake package identifier (e.g.
    /// `lean_rs_fixture`); `module` is the dotted Lean module name (e.g.
    /// `LeanRsFixture.Scalars`).
    ///
    /// # Errors
    ///
    /// Returns [`LeanError::Host`] with stage [`HostStage::Link`] if the
    /// package or any module component is empty or contains characters
    /// outside the conservative `[A-Za-z_][A-Za-z0-9_]*` subset accepted
    /// by Lake's symbol mangler.
    pub(crate) fn from_lake_names(package: &str, module: &str) -> LeanResult<Self> {
        validate_component(package, "package name")?;
        let mut display = String::new();
        display.push_str(package);
        display.push_str("::");
        display.push_str(module);

        let mut mangled = String::new();
        mangled.push_str("initialize_");
        push_mangled(&mut mangled, package);
        for component in module.split('.') {
            validate_component(component, "module component")?;
            mangled.push('_');
            push_mangled(&mut mangled, component);
        }
        // `CString::new` rejects interior NULs; the validator above
        // already excluded `'\0'`, so this only fails on bug-level input.
        let symbol = CString::new(mangled)
            .map_err(|_| LeanError::linking("internal: mangled initializer symbol contained NUL byte"))?;
        Ok(Self { symbol, display })
    }

    /// C-string-ready symbol bytes (with the trailing NUL) for
    /// `libloading::Library::get`.
    pub(crate) fn symbol_bytes(&self) -> &[u8] {
        self.symbol.as_bytes_with_nul()
    }

    /// Symbol bytes without the trailing NUL — for diagnostics that
    /// embed the raw C name in a message.
    pub(crate) fn symbol_str(&self) -> &str {
        // SAFETY: `from_lake_names` only constructs symbols from ASCII
        // bytes (the validator restricts components to ASCII), and
        // `CString::as_bytes()` excludes the NUL; the result is valid
        // UTF-8 by construction.
        unsafe { std::str::from_utf8_unchecked(self.symbol.as_bytes()) }
    }

    /// Human-readable `package::Module.Path` form for diagnostics.
    pub(crate) fn display(&self) -> &str {
        &self.display
    }
}

/// Append `component` to `out`, doubling every literal `_` per Lake's
/// mangling convention.
fn push_mangled(out: &mut String, component: &str) {
    for ch in component.chars() {
        if ch == '_' {
            out.push_str("__");
        } else {
            out.push(ch);
        }
    }
}

/// Reject empty components and any character outside Lake's accepted
/// `[A-Za-z_][A-Za-z0-9_]*` per-component alphabet.
fn validate_component(component: &str, kind: &str) -> LeanResult<()> {
    if component.is_empty() {
        return Err(LeanError::linking(format!("invalid {kind}: empty component")));
    }
    let mut chars = component.chars();
    let first = chars.next().unwrap_or('\0');
    if !is_ident_start(first) {
        return Err(LeanError::linking(format!(
            "invalid {kind} '{component}': first character must be ASCII letter or underscore"
        )));
    }
    for ch in chars {
        if !is_ident_continue(ch) {
            return Err(LeanError::linking(format!(
                "invalid {kind} '{component}': character {ch:?} is not in [A-Za-z0-9_]"
            )));
        }
    }
    Ok(())
}

fn is_ident_start(ch: char) -> bool {
    ch == '_' || ch.is_ascii_alphabetic()
}

fn is_ident_continue(ch: char) -> bool {
    ch == '_' || ch.is_ascii_alphanumeric()
}

/// `builtin = 1` selects runtime-mode initialization (install the
/// module's persistent objects and runtime tables). `builtin = 0` is
/// the embedding/compile-time path Lean's own compiler uses when
/// statically linking a module into a new build; `lean-rs` always runs
/// as an embedder loading already-compiled artifacts, so the runtime
/// path is the only correct choice.
const BUILTIN_RUNTIME: u8 = 1;

/// Raw signature emitted by Lake for every module initializer at Lean
/// 4.29.1. Single argument; returns an owned `IO Unit` result.
pub(crate) type RawInitializer = unsafe extern "C" fn(u8) -> *mut lean_object;

/// Run a module initializer under a panic boundary and classify any
/// `IO` failure as a [`HostStage::Load`] host error.
///
/// The Lean static `_G_initialized` makes the second and later calls
/// for the same module a cheap `IO.ok(())` return; this function is
/// safe to call repeatedly without bookkeeping on the Rust side.
pub(crate) fn call_initializer(runtime: &LeanRuntime, init: RawInitializer, name: &InitializerName) -> LeanResult<()> {
    let _span = tracing::debug_span!(
        target: "lean_rs",
        "lean_rs.module.initializer.call",
        initializer = name.display(),
    )
    .entered();
    // SAFETY: `init` is the function pointer freshly resolved from the
    // owning `LeanLibrary`'s `dlsym`; the library is borrowed for the
    // duration of this call, so the pointer is valid. Lean module
    // initializers do not call back into Rust today, but we wrap the
    // FFI call in `catch_unwind` as a defence-in-depth boundary
    // consistent with `runtime::init::do_initialize_once`.
    let outcome = panic::catch_unwind(AssertUnwindSafe(|| unsafe { init(BUILTIN_RUNTIME) }));
    let raw_result = match outcome {
        Ok(ptr) => ptr,
        Err(payload) => {
            return Err(LeanError::module_init_panic(payload.as_ref()));
        }
    };
    let Some(non_null) = NonNull::new(raw_result) else {
        return Err(LeanError::module_init(format!(
            "module '{}' initializer returned a null IO result pointer",
            name.display()
        )));
    };
    // SAFETY: `init` returns an owned `lean_obj_res` (an `IO α` value)
    // per Lake's codegen contract. We just witnessed it is non-null;
    // wrapping in `Obj` transfers the one reference count, and `Drop`
    // (via `decode_io`'s consumption) releases it.
    let result = unsafe { Obj::from_owned_raw(runtime, non_null.as_ptr()) };
    match decode_io(runtime, result) {
        Ok(unit_obj) => {
            // The success value is `lean_box(0)` (Unit); dropping the
            // `Obj` releases it via `lean_dec`, which short-circuits on
            // scalar-tagged pointers.
            drop(unit_obj);
            Ok(())
        }
        Err(LeanError::LeanException(exc)) => Err(LeanError::module_init(format!(
            "module '{}' initializer raised: {}",
            name.display(),
            exc.message()
        ))),
        Err(other) => Err(other),
    }
}
