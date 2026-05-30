//! `LeanModule::exported_unchecked` is `unsafe fn`: arbitrary dynamic-export
//! lookup cannot be validated from a raw symbol name plus caller-chosen
//! `Args`/`R`, so the call is memory-safe only if the symbol's compiled C ABI
//! is known to match those Rust types. Calling it outside an `unsafe` block
//! must be rejected (E0133). The manifest-checked, safe alternative is
//! `LeanCapability::exported`, which needs no `unsafe`.

#![allow(dead_code)]

use lean_rs::{LeanIo, LeanModule};

fn use_unchecked(module: &LeanModule<'static, 'static>) {
    // No `unsafe` block. `()` implements `LeanArgs` and `LeanIo<u64>`
    // implements `DecodeCallResult`, so the *only* error is the missing
    // `unsafe`—not an unsatisfied trait bound.
    let _exported = module.exported_unchecked::<(), LeanIo<u64>>("f");
}

fn main() {}
