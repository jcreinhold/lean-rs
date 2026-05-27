//! Typed handles for exported Lean functions.
//!
//! The public dispatch surface has two lookup paths:
//!
//! - [`LeanCapability::exported<Args, R>(name)`](super::capability::LeanCapability::exported)
//!   —safe checked lookup for capabilities opened from trusted manifest
//!   signature metadata.
//! - [`LeanModule::exported_unchecked<Args, R>(name)`](super::loaded::LeanModule::exported_unchecked)
//!   —unsafe arbitrary lookup for callers that can prove the symbol's
//!   compiled C ABI matches the requested Rust `Args`/`R` shape.
//!
//! The typed handle surface is two types and three sealed traits:
//!
//! - [`LeanExported<'lean, 'lib, Args, R>`]—a typed handle for an
//!   exported Lean symbol. `Args` is a tuple of Rust argument types
//!   (`()`, `(A,)`, `(A, B)`, … up to arity 12); each element must
//!   implement [`crate::abi::traits::LeanAbi`]. `R` is the return type, bounded
//!   by [`DecodeCallResult`]—a sealed trait satisfied by every
//!   [`crate::abi::traits::LeanAbi`] type (pure call) and by [`LeanIo<T>`] for
//!   `T: crate::abi::traits::TryFromLean` (IO-returning Lean export).
//! - [`LeanIo<T>`]—return-type marker for Lean exports declared
//!   `IO α`. Writing `exported_unchecked::<Args, LeanIo<T>>(name)` tells the
//!   handle to compose `decode_io` before
//!   [`crate::abi::traits::TryFromLean::try_from_lean`]. The `.call(...)` method
//!   returns `LeanResult<T>` (not `LeanResult<LeanIo<T>>`)—the marker
//!   only lives in the type signature.
//!
//! `LeanModule::exported_unchecked` distinguishes function-symbol
//! resolution from Lean nullary-constant global reads transparently,
//! using the `globals` set computed at
//! [`super::library::LeanLibrary::open`].
//!
//! ## Call shape
//!
//! ```ignore
//! let runtime = lean_rs::LeanRuntime::init()?;
//! let library = lean_rs::module::LeanLibrary::open(runtime, path)?;
//! let module  = library.initialize_module("foo_pkg", "Foo.Bar")?;
//!
//! // Pure, arity 1:
//! // SAFETY: the Lean export's C ABI is known to be `String -> String`.
//! let f = unsafe { module.exported_unchecked::<(String,), String>("foo_string_identity") }?;
//! let s = f.call("abc".to_owned())?;
//!
//! // IO, arity 0; return type carries the marker, .call returns T directly:
//! // SAFETY: the Lean export's C ABI is known to be `IO UInt64`.
//! let g = unsafe { module.exported_unchecked::<(), lean_rs::module::LeanIo<u64>>("foo_io_seven") }?;
//! let n: u64 = g.call()?;
//! ```
//!
//! ## Design rationale
//!
//! A single tuple-`Args` handle replaces an arity-stamped
//! `LeanExported0..LeanExported12` family. Arity lives in the tuple
//! type, not in the method name. IO-ness lives in the return type rather
//! than in a `.call_io()` method. Per-type C-ABI representation (unboxed
//! scalar vs boxed `lean_object*`) is hidden behind [`crate::abi::traits::LeanAbi`]
//!—Lake emits both shapes depending on the Lean type, and the typed
//! handle's function-pointer cast is generic over each arg's `CRepr`.
//!
//! Lake's compiled `IO α` exports take only the user-visible arguments at
//! the C ABI; the "world" is a Lean-level abstraction the compiler
//! optimises away for top-level IO exports, so `.call` synthesises no
//! world token.
//!
//! ## Checked lookup boundary
//!
//! `LeanCapability::exported` is safe because the capability manifest
//! supplies the trusted ABI fact before the typed handle is built.
//! `LeanModule::exported_unchecked` remains unsafe because a raw symbol
//! name plus caller-chosen generic types cannot establish that fact.
//! Raw dynamic-loader addresses are intentionally not part of the
//! public API.

// SAFETY DOC: every `unsafe { ... }` block in this file carries its own
// `// SAFETY:` comment. The blanket allow exists because this is the
// single unchecked-front-door site that resolves and dispatches
// user-typed Lean exports, per `docs/architecture/01-safety-model.md`.
#![allow(unsafe_code)]
// The public surface intentionally references `pub(crate)` items
// (`Obj`) inside trait method signatures. Soundness is enforced by
// sealing (`sealed::SealedArgs` / `sealed::SealedResult`); downstream
// cannot add `LeanArgs` or `DecodeCallResult` impls, so the
// visibility-of-bounds lints are a documentation concern that does not
// apply here.
#![allow(private_bounds, private_interfaces)]

use core::ffi::c_void;
use core::marker::PhantomData;

use lean_rs_sys::lean_object;
use lean_rs_sys::refcount::lean_inc;
use lean_toolchain::{
    LeanExportAbiRepr, LeanExportArgAbi, LeanExportOwnership, LeanExportResultConvention, LeanExportReturnAbi,
};

use super::library::LeanLibrary;
use super::loaded::LeanModule;
use crate::abi::traits::{LeanAbi, LeanCReprAbi, TryFromLean};
#[cfg(doc)]
use crate::error::HostStage;
use crate::error::io::decode_io;
use crate::error::{LeanError, LeanResult};
use crate::runtime::LeanRuntime;
use crate::runtime::obj::Obj;

/// A typed handle for an exported Lean symbol.
///
/// Holds a resolved symbol address (function or persistent global) and
/// the runtime borrow that anchors any `Obj` it produces. Construction
/// goes exclusively through
/// [`LeanModule::exported_unchecked`](super::loaded::LeanModule::exported_unchecked); the
/// handle borrows from its source [`LeanModule`] via `'lib` so neither
/// the library nor the runtime can be dropped while a handle is live.
///
/// `Args` is a tuple of Rust argument types whose elements implement
/// [`LeanAbi`]; the [`LeanArgs`] impl for that tuple supplies the
/// arity. `R` is the return type, satisfying [`DecodeCallResult`].
///
/// Neither [`Send`] nor [`Sync`]: the contained runtime reference and
/// the `*mut` symbol address both propagate the per-thread restriction.
pub struct LeanExported<'lean, 'lib, Args, R> {
    target: CallableTarget,
    runtime: &'lean LeanRuntime,
    _life: PhantomData<&'lib LeanLibrary<'lean>>,
    _args: PhantomData<fn(Args) -> R>,
}

/// Return-type marker for Lean exports declared `IO α`.
///
/// Writing `exported_unchecked::<Args, LeanIo<T>>(name)` makes [`LeanExported`]'s
/// `.call` method compose `decode_io` before
/// `TryFromLean::try_from_lean`, so the handle returns `LeanResult<T>`.
/// The marker has no value—it is a pure type-level switch.
///
/// `LeanIo<T>` cannot be constructed from outside the crate (its single
/// field is private); it appears only in `R` positions on
/// [`LeanModule::exported_unchecked`](super::loaded::LeanModule::exported_unchecked).
pub struct LeanIo<T>(PhantomData<fn() -> T>);

/// Internal: which symbol shape the handle dispatches.
///
/// Lean compiles `def x : T := constant` to a persistent
/// `lean_object*` data-section symbol (`lean_mark_persistent` at module
/// init); the `Global` variant carries the address of that storage.
/// Every other `@[export]` is a callable function whose entry point is
/// the symbol's address.
enum CallableTarget {
    /// Symbol resolves to a function entry point.
    Function(*mut c_void),
    /// Symbol resolves to the storage of a persistent `lean_object*`.
    Global(*mut *mut lean_object),
}

// -- Sealing: prevent downstream impls of LeanArgs / DecodeCallResult ----

/// Private supertraits that seal [`LeanArgs`] and [`DecodeCallResult`]
/// at the crate boundary.
///
/// Two distinct sealed traits (rather than one shared `Sealed`) because
/// `()`, `(u64,)`, and other tuples implement [`TryFromLean`]—a single
/// `Sealed` blanket-implemented over `T: TryFromLean` would overlap with
/// any per-arity `Sealed` impl on tuples. Splitting the seal by which
/// public trait it gates removes the overlap.
mod sealed {
    /// Sealed supertrait for [`super::LeanArgs`].
    #[allow(unreachable_pub, reason = "sealed trait pattern—pub inside a pub(crate) module")]
    pub trait SealedArgs {}
    /// Sealed supertrait for [`super::DecodeCallResult`].
    #[allow(unreachable_pub, reason = "sealed trait pattern—pub inside a pub(crate) module")]
    pub trait SealedResult {}
}

// -- LeanArgs: arity marker on argument tuples ---------------------------

/// Per-arity marker for [`LeanExported`]'s argument tuple.
///
/// Sealed via `SealedArgs`; downstream cannot implement it.
/// Macro-stamped for arity-0..=12 tuples whose elements implement
/// [`LeanAbi`]. Used at lookup time to reject `ARITY > 0` against a
/// Lean nullary-constant global.
pub trait LeanArgs<'lean>: Sized + sealed::SealedArgs {
    /// Number of arguments the tuple represents.
    const ARITY: usize;

    /// ABI metadata for this argument tuple.
    #[doc(hidden)]
    fn export_abi_args() -> Vec<LeanExportArgAbi>;

    /// Destructure `args` and dispatch through `handle`.
    ///
    /// The per-arity `.call(a1, a2, ...)` method on [`LeanExported`]
    /// takes its arguments destructured (one per parameter) because
    /// that is the natural ergonomic form for hand-written call sites.
    /// Generic callers cannot destructure a generic `Args` tuple, so
    /// they reach `.call(...)` through this associated function instead.
    /// Macro-stamped per arity to forward to the existing destructured
    /// impl.
    #[doc(hidden)]
    fn invoke<R>(handle: &LeanExported<'lean, '_, Self, R>, args: Self) -> LeanResult<R::Output>
    where
        R: DecodeCallResult<'lean>;
}

// -- DecodeCallResult: pure vs IO return decoding ------------------------

/// How to decode an owned Lean call result into a Rust value.
///
/// Sealed via `SealedResult`; downstream cannot implement it.
/// Two implementors:
///
/// - blanket `impl<T: LeanAbi<'lean>> DecodeCallResult<'lean> for T`—
///   the *pure* path; `CRepr = T::CRepr`, `Output = T`; `decode_c` is
///   `T::from_c(c, rt)`.
/// - special `impl<T: TryFromLean<'lean>> DecodeCallResult<'lean> for
///   LeanIo<T>`—the *IO* path; `CRepr = *mut lean_object` (the IO
///   result wrapper), `Output = T`; `decode_c` wraps the pointer in
///   `Obj`, runs `decode_io`, then `T::try_from_lean`.
///
/// Coherence holds because [`LeanIo<T>`] does not implement [`LeanAbi`],
/// so the blanket impl does not match it.
pub trait DecodeCallResult<'lean>: Sized + sealed::SealedResult {
    /// What `.call(...)` returns inside `LeanResult`.
    type Output;
    /// The C-ABI return type of the Lake-emitted function. For the pure
    /// path this is `T::CRepr` (e.g. `u8` for `Bool` exports, `*mut
    /// lean_object` for `String`); for the IO path it is always
    /// `*mut lean_object` (the `lean_io_result_*` wrapper).
    type CRepr: Copy;
    /// `true` iff this decoder expects a `lean_io_result_*` shape.
    /// Used at lookup time to reject `LeanIo<_>` against a global symbol
    /// (which is never IO-typed in Lean's compilation).
    #[doc(hidden)]
    const EXPECTS_IO_RESULT: bool;
    /// ABI metadata for this return decoder.
    #[doc(hidden)]
    fn export_abi_return() -> LeanExportReturnAbi;
    /// Decode the owned C-ABI return value into [`Output`](Self::Output).
    ///
    /// # Errors
    ///
    /// Returns whatever the underlying decoder returns—
    /// [`LeanAbi::from_c`] for the pure path, `decode_io` chained into
    /// `TryFromLean::try_from_lean` for the IO path.
    #[doc(hidden)]
    fn decode_c(c: Self::CRepr, runtime: &'lean LeanRuntime) -> LeanResult<Self::Output>;
}

impl<'lean, T> sealed::SealedResult for T where T: LeanAbi<'lean> {}
impl<'lean, T> DecodeCallResult<'lean> for T
where
    T: LeanAbi<'lean>,
{
    type Output = T;
    type CRepr = T::CRepr;
    const EXPECTS_IO_RESULT: bool = false;
    fn export_abi_return() -> LeanExportReturnAbi {
        export_abi_return_for::<T::CRepr>(LeanExportResultConvention::Pure)
    }
    fn decode_c(c: T::CRepr, runtime: &'lean LeanRuntime) -> LeanResult<T> {
        T::from_c(c, runtime)
    }
}

impl<T> sealed::SealedResult for LeanIo<T> {}
impl<'lean, T> DecodeCallResult<'lean> for LeanIo<T>
where
    T: TryFromLean<'lean>,
{
    type Output = T;
    type CRepr = *mut lean_object;
    const EXPECTS_IO_RESULT: bool = true;
    fn export_abi_return() -> LeanExportReturnAbi {
        LeanExportReturnAbi::new(
            LeanExportAbiRepr::LeanObject,
            LeanExportOwnership::Owned,
            LeanExportResultConvention::IoResult,
        )
    }
    #[allow(
        clippy::not_unsafe_ptr_arg_deref,
        reason = "sealed trait—caller invariant documented on DecodeCallResult::decode_c"
    )]
    fn decode_c(c: *mut lean_object, runtime: &'lean LeanRuntime) -> LeanResult<T> {
        // SAFETY: `c` is an owned `lean_io_result_*` returned by an
        // extern Lean function; `runtime` witnesses `'lean`.
        let result_obj = unsafe { Obj::from_owned_raw(runtime, c) };
        let payload = decode_io(runtime, result_obj)?;
        T::try_from_lean(payload)
    }
}

pub(crate) fn export_abi_arg_for<T: LeanCReprAbi>() -> LeanExportArgAbi {
    let repr = T::EXPORT_ABI_REPR;
    LeanExportArgAbi::new(repr, ownership_for_repr(repr))
}

fn export_abi_return_for<T: LeanCReprAbi>(convention: LeanExportResultConvention) -> LeanExportReturnAbi {
    let repr = T::EXPORT_ABI_REPR;
    LeanExportReturnAbi::new(repr, ownership_for_repr(repr), convention)
}

fn ownership_for_repr(repr: LeanExportAbiRepr) -> LeanExportOwnership {
    if repr == LeanExportAbiRepr::LeanObject {
        LeanExportOwnership::Owned
    } else {
        LeanExportOwnership::None
    }
}

// -- LeanExported<Args, R>: Debug impl + Global-path helper --------------

impl<Args, R> core::fmt::Debug for LeanExported<'_, '_, Args, R> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let kind = match self.target {
            CallableTarget::Function(_) => "Function",
            CallableTarget::Global(_) => "Global",
        };
        f.debug_struct("LeanExported").field("kind", &kind).finish()
    }
}

/// Read a Lean nullary-constant global's persistent `*mut lean_object`,
/// `lean_inc` it, and return the bumped pointer.
///
/// The same logic appears in the arity-0 stamped `.call` impl on the
/// Global branch—extracted here so the per-arity macro stays small.
///
/// # Safety
///
/// `ptr` must point at a Lake-emitted persistent `lean_object*` slot
/// (data-section export). The caller has verified this through the
/// `globals` set at `LeanLibrary::open` time.
unsafe fn read_global_pointer(ptr: *mut *mut lean_object) -> *mut lean_object {
    // SAFETY: `ptr` is a Lake-installed persistent slot (per caller
    // invariant). Reading the slot yields the persistent
    // `lean_object*`; `lean_inc` bumps its refcount so the returned
    // value owns a fresh reference that `Drop` can release.
    unsafe {
        let inner = *ptr;
        lean_inc(inner);
        inner
    }
}

// -- LeanModule::exported_unchecked lookup -----------------------------------------

// Both lifetimes flow into the returned `LeanExported<'lean, 'lib, ...>`.
#[allow(single_use_lifetimes, reason = "'lean and 'lib both anchor the returned handle")]
impl<'lean, 'lib> LeanModule<'lean, 'lib> {
    /// Look up a typed handle for the named exported symbol without ABI
    /// metadata checks.
    ///
    /// `Args` is a tuple of Rust argument types whose arity matches the
    /// Lean export's parameter count (use `()` for nullary exports);
    /// each element must implement [`LeanAbi`]. `R` is the return
    /// decoder: either a [`LeanAbi`] type (pure path) or [`LeanIo<T>`]
    /// for an `IO α`-returning export.
    ///
    /// # Safety
    ///
    /// The caller must prove that `name` resolves to a Lean export whose
    /// emitted C ABI and ownership behavior exactly match the requested
    /// Rust `Args` and `R`:
    ///
    /// - each argument slot must have the C representation selected by
    ///   that slot's [`LeanAbi::CRepr`], in the same order and arity;
    /// - the return slot must have the C representation expected by
    ///   [`DecodeCallResult::CRepr`], including `LeanIo<T>` only for
    ///   exports that return Lean `IO α`;
    /// - object arguments and results must follow Lake's ownership and
    ///   refcount transfer rules for the chosen signature.
    ///
    /// Passing a Rust signature that does not match the Lean export's
    /// actual C ABI is undefined behavior.
    ///
    /// # Errors
    ///
    /// Returns [`LeanError::Host`] with stage [`HostStage::Link`] if:
    ///
    /// - the symbol is not exported by this module's library;
    /// - the symbol is a Lean nullary-constant global (data-section
    ///   symbol) and `Args::ARITY > 0`—the function-vs-global mismatch
    ///   is diagnosed at lookup so the next `.call` cannot SIGBUS;
    /// - the symbol is a Lean nullary-constant global and `R` is
    ///   [`LeanIo<_>`]—globals are never IO-returning in Lean, so the
    ///   decoder cannot apply.
    pub unsafe fn exported_unchecked<Args, R>(&self, name: &str) -> LeanResult<LeanExported<'lean, 'lib, Args, R>>
    where
        Args: LeanArgs<'lean>,
        R: DecodeCallResult<'lean>,
    {
        let library = self.library();
        let target = if library.globals().contains(name) {
            if Args::ARITY > 0 {
                return Err(LeanError::symbol_lookup(format!(
                    "exported symbol '{}' in '{}' is a Lean nullary-constant global, not a function; \
                     look it up with `exported_unchecked::<(), R>(name)` instead (lookup arity is {})",
                    name,
                    library.path().display(),
                    Args::ARITY,
                )));
            }
            if R::EXPECTS_IO_RESULT {
                return Err(LeanError::symbol_lookup(format!(
                    "exported symbol '{}' in '{}' is a Lean nullary-constant global; \
                     `LeanIo<_>` does not apply (Lean does not compile globals as IO-returning)",
                    name,
                    library.path().display(),
                )));
            }
            let ptr = library.resolve_global_symbol(name)?;
            CallableTarget::Global(ptr)
        } else {
            let addr = library.resolve_function_symbol(name)?;
            CallableTarget::Function(addr)
        };
        Ok(LeanExported {
            target,
            runtime: library.runtime(),
            _life: PhantomData,
            _args: PhantomData,
        })
    }
}

// -- impl_arity!: stamps LeanArgs + LeanExported::call for one arity -----

/// Stamp the [`LeanArgs`] impl, the `SealedArgs` seal, and the
/// per-arity [`LeanExported`] `.call` impl for one arity.
///
/// Invoked once per arity 0..=12. Each invocation spells out the
/// per-slot `<$A as LeanAbi<'lean>>::CRepr` arguments in the function
/// pointer signature—stable Rust has no token-counting expansion that
/// would let us synthesise N copies of the type from a count.
///
/// The macro takes only the typed-arg list `(A1 a1, ..., AN aN)` plus
/// the arity literal. The function-pointer type is constructed inline
/// inside the `.call` body using the per-`A` associated types.
macro_rules! impl_arity {
    (
        $arity:literal,
        ($($A:ident $a:ident),* $(,)?)
    ) => {
        impl<$($A,)*> sealed::SealedArgs for ($($A,)*) {}

        // The `'lean` parameter binds the per-arg `LeanAbi` bounds even
        // when the tuple itself is arity 0 (no $A to use it explicitly).
        #[allow(single_use_lifetimes, reason = "binds the LeanAbi<'lean> bounds")]
        impl<'lean, $($A,)*> LeanArgs<'lean> for ($($A,)*)
        where
            $($A: LeanAbi<'lean>,)*
        {
            const ARITY: usize = $arity;

            fn export_abi_args() -> Vec<LeanExportArgAbi> {
                vec![$(export_abi_arg_for::<<$A as LeanAbi<'lean>>::CRepr>()),*]
            }

            #[allow(
                clippy::unused_unit,
                unused_variables,
                reason = "arity 0 has no destructure target and no $A to bind"
            )]
            fn invoke<R>(
                handle: &LeanExported<'lean, '_, Self, R>,
                args: Self,
            ) -> LeanResult<R::Output>
            where
                R: DecodeCallResult<'lean>,
            {
                let ($($a,)*) = args;
                handle.call($($a,)*)
            }
        }

        // Both lifetimes flow into LeanExported<'lean, 'lib, ...>.
        #[allow(single_use_lifetimes, reason = "'lean and 'lib anchor LeanExported")]
        impl<'lean, 'lib, $($A,)* R> LeanExported<'lean, 'lib, ($($A,)*), R>
        where
            $($A: LeanAbi<'lean>,)*
            R: DecodeCallResult<'lean>,
        {
            /// Invoke the exported symbol and decode the result.
            ///
            /// # Errors
            ///
            /// Returns [`LeanError::LeanException`] when the underlying
            /// Lean export raises through `IO` (only possible when `R`
            /// is [`LeanIo<_>`]). Returns [`LeanError::Host`] with stage
            /// [`HostStage::Conversion`] when the return value does not
            /// decode into the declared `R` type.
            #[allow(
                clippy::unused_unit,
                unused_variables,
                reason = "arity 0 does not convert args or destructure them"
            )]
            pub fn call(&self, $($a: $A),*) -> LeanResult<R::Output> {
                // Debug-only: catch worker threads that forgot to construct
                // a `LeanThreadGuard` before invoking Lean code. Compiles
                // to a no-op in release. See
                // `docs/architecture/04-concurrency.md`.
                crate::runtime::thread::debug_assert_attached("LeanExported::call");
                let _span = tracing::trace_span!(
                    target: "lean_rs",
                    "lean_rs.module.exported.call",
                    arity = $arity,
                )
                .entered();
                let runtime = self.runtime;
                let raw_out: R::CRepr = match self.target {
                    CallableTarget::Function(addr) => {
                        // The function-pointer signature is per-arg
                        // `<$A as LeanAbi<'lean>>::CRepr` (an unboxed
                        // scalar or a `*mut lean_object`, depending on
                        // the type) and the return is `R::CRepr` (which
                        // is `T::CRepr` for the pure path and
                        // `*mut lean_object` for `LeanIo<T>`).
                        //
                        // SAFETY: lookup verified the symbol resolves to
                        // a function entry point in the loaded library;
                        // the tuple type carries the matching arity and
                        // per-arg CRepr so the transmute lines up with
                        // Lake's emitted C signature.
                        let f = unsafe {
                            core::mem::transmute::<
                                *mut c_void,
                                unsafe extern "C" fn(
                                    $(<$A as LeanAbi<'lean>>::CRepr,)*
                                ) -> <R as DecodeCallResult<'lean>>::CRepr,
                            >(addr)
                        };
                        // Each $a: $A converts to its CRepr through
                        // LeanAbi::into_c, transferring ownership of any
                        // allocated Lean object.
                        $(let $a = $a.into_c(runtime);)*
                        // SAFETY: per-arg ownership transferred per
                        // Lake's `lean_obj_arg` contract; the return
                        // value owns one refcount (or is a scalar—no
                        // refcount obligation).
                        unsafe { f($($a,)*) }
                    }
                    CallableTarget::Global(ptr) => {
                        debug_assert_eq!(
                            <($($A,)*) as LeanArgs<'lean>>::ARITY,
                            0,
                            "arity > 0 against a global is rejected at lookup",
                        );
                        debug_assert!(
                            !<R as DecodeCallResult<'lean>>::EXPECTS_IO_RESULT,
                            "LeanIo<_> against a global is rejected at lookup",
                        );
                        // `R::CRepr` for the global path must be
                        // `*mut lean_object` because the pure-path
                        // blanket impl picks `T::CRepr`, and any boxed
                        // `T: LeanAbi` has `CRepr = *mut lean_object`.
                        // Globals always hold a `lean_object*`, so this
                        // alignment is structural. transmute_copy is
                        // sound because Rust statically guarantees both
                        // are pointer-sized when we reach this branch.
                        // SAFETY: pointer-sized scalar reinterpret;
                        // `read_global_pointer` returns a non-null
                        // `*mut lean_object` owning one refcount.
                        let raw: *mut lean_object = unsafe { read_global_pointer(ptr) };
                        // SAFETY: R::CRepr is the pointer-sized C ABI
                        // type the pure-path blanket assigned. For
                        // `Obj<'lean>`, `Option<T>`, `String`, etc.,
                        // this is `*mut lean_object` directly.
                        unsafe { core::mem::transmute_copy::<*mut lean_object, R::CRepr>(&raw) }
                    }
                };
                R::decode_c(raw_out, runtime)
            }
        }
    };
}

impl_arity!(0, ());
impl_arity!(1, (A1 a1));
impl_arity!(2, (A1 a1, A2 a2));
impl_arity!(3, (A1 a1, A2 a2, A3 a3));
impl_arity!(4, (A1 a1, A2 a2, A3 a3, A4 a4));
impl_arity!(5, (A1 a1, A2 a2, A3 a3, A4 a4, A5 a5));
impl_arity!(6, (A1 a1, A2 a2, A3 a3, A4 a4, A5 a5, A6 a6));
impl_arity!(7, (A1 a1, A2 a2, A3 a3, A4 a4, A5 a5, A6 a6, A7 a7));
impl_arity!(8, (A1 a1, A2 a2, A3 a3, A4 a4, A5 a5, A6 a6, A7 a7, A8 a8));
impl_arity!(9, (A1 a1, A2 a2, A3 a3, A4 a4, A5 a5, A6 a6, A7 a7, A8 a8, A9 a9));
impl_arity!(10, (A1 a1, A2 a2, A3 a3, A4 a4, A5 a5, A6 a6, A7 a7, A8 a8, A9 a9, A10 a10));
impl_arity!(11, (A1 a1, A2 a2, A3 a3, A4 a4, A5 a5, A6 a6, A7 a7, A8 a8, A9 a9, A10 a10, A11 a11));
impl_arity!(
    12,
    (A1 a1, A2 a2, A3 a3, A4 a4, A5 a5, A6 a6, A7 a7, A8 a8, A9 a9, A10 a10, A11 a11, A12 a12)
);
