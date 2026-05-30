//! `LeanIo<T>` is a pure type-level marker for Lean exports declared `IO α`.
//! It has no value—its single field is private—so downstream code cannot
//! construct one; it may only appear in `R` positions on
//! `LeanModule::exported_unchecked`.

use lean_rs::LeanIo;

fn main() {
    // The tuple field is private: constructing `LeanIo` from outside the
    // crate is a privacy error.
    let _io = LeanIo::<u64>(core::marker::PhantomData);
}
