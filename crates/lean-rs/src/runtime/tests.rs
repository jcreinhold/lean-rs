//! `cargo test -p lean-rs --lib runtime::tests`
//!
//! These tests share a process: Lean's runtime initializes exactly once
//! across the whole test binary thanks to the `OnceLock` cell in
//! [`super::init`]. The tests below assert Rust-visible idempotence and
//! exercise the worker-thread attach path; they do not (and cannot) prove
//! that the underlying C-level state initialized exactly once—that
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
        // The guard's `Drop` is the property under test—letting it run
        // at scope exit must not panic or abort.
        drop(guard);
    });
    handle.join().expect("worker thread must not panic");
}

#[test]
#[cfg(debug_assertions)]
fn debug_assert_attached_panics_on_unattached_worker() {
    use super::thread::debug_assert_attached;

    // Every successful `init()` lifts the per-thread attach depth, but
    // the worker spawned below never calls `init()` and never constructs
    // a `LeanThreadGuard`. Its TLS depth stays at zero, so the
    // assertion must fire.
    let _ = LeanRuntime::init().expect("main-thread init must succeed");
    let handle = thread::spawn(|| {
        debug_assert_attached("tests::debug_assert_attached_panics_on_unattached_worker");
    });
    let err = handle
        .join()
        .expect_err("worker without a guard must panic the debug assertion");
    drop(err);
}

#[test]
fn attach_guard_satisfies_debug_assert_attached() {
    use super::thread::debug_assert_attached;

    let _ = LeanRuntime::init().expect("main-thread init must succeed");
    let handle = thread::spawn(|| {
        let runtime = LeanRuntime::init().expect("worker init must succeed");
        let _guard = LeanThreadGuard::attach(runtime);
        // No panic: the guard pushed the per-thread attach depth to 1.
        debug_assert_attached("tests::attach_guard_satisfies_debug_assert_attached");
    });
    handle.join().expect("worker thread must not panic");
}

#[test]
fn nested_attach_guards_balance_depth() {
    use super::thread::debug_assert_attached;

    let _ = LeanRuntime::init().expect("main-thread init must succeed");
    let handle = thread::spawn(|| {
        let runtime = LeanRuntime::init().expect("worker init must succeed");
        let outer = LeanThreadGuard::attach(runtime);
        {
            let inner = LeanThreadGuard::attach(runtime);
            debug_assert_attached("nested outer + inner");
            drop(inner);
        }
        // Inner dropped; outer still keeps us attached.
        debug_assert_attached("nested outer only");
        drop(outer);
    });
    handle.join().expect("worker thread must not panic");
}
