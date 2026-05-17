//! `cargo test -p lean-rs --lib runtime::tests`
//!
//! These tests share a process: Lean's runtime initializes exactly once
//! across the whole test binary thanks to the `OnceLock` cell in
//! [`super::init`]. The tests below assert Rust-visible idempotence and
//! exercise the worker-thread attach path; they do not (and cannot) prove
//! that the underlying C-level state initialized exactly once — that
//! property is delegated to `OnceLock`.

#![allow(clippy::expect_used)]

use std::ptr;
use std::thread;

use super::LeanRuntime;
use super::thread::LeanThreadGuard;

#[test]
fn init_returns_an_ok_borrow() {
    let runtime = LeanRuntime::init().expect("first init must succeed");
    // The borrow is non-null and addressable; we do not depend on the
    // exact pointer value because it is the implementation-defined ZST
    // dangling pointer.
    let _ = ptr::from_ref(runtime);
}

#[test]
fn init_is_idempotent_across_repeated_calls() {
    let first = LeanRuntime::init().expect("first init must succeed");
    let second = LeanRuntime::init().expect("second init must succeed");
    let third = LeanRuntime::init().expect("third init must succeed");
    // All three calls must return the same borrow. `ptr::eq` is the right
    // tool: it compares addresses, which is the operational identity for
    // a ZST handle that has no fields to compare.
    assert!(ptr::eq(first, second));
    assert!(ptr::eq(second, third));
}

#[test]
fn worker_thread_can_attach_and_finalize() {
    // Ensure the runtime is up before the worker starts. The worker also
    // calls `init()` itself; the second call returns the cached state
    // without re-entering the C initialization.
    let _ = LeanRuntime::init().expect("main-thread init must succeed");
    let handle = thread::spawn(|| {
        let runtime = LeanRuntime::init().expect("worker init must succeed");
        let guard = LeanThreadGuard::attach(runtime);
        // The guard's `Drop` is the property under test — letting it run
        // at scope exit must not panic or abort.
        drop(guard);
    });
    handle.join().expect("worker thread must not panic");
}
