//! Bounded process-wide admission for Lean import and source operations.
use core::cell::Cell;
use core::marker::PhantomData;
use std::rc::Rc;
use std::sync::{Mutex, TryLockError};
use std::time::{Duration, Instant};

use crate::LeanResult;

static IMPORT_LOCK: Mutex<()> = Mutex::new(());

/// Exclusive, thread-confined access to the current import transaction.
pub struct HostImportTransaction {
    _thread: PhantomData<Rc<()>>,
}

/// Run one transaction spanning native operations and their cache or epoch updates.
///
/// # Errors
///
/// Returns a resource error on recursive admission or after thirty seconds of contention.
pub fn with_host_import_transaction<T>(f: impl FnOnce(&mut HostImportTransaction) -> LeanResult<T>) -> LeanResult<T> {
    with_import_lock(&IMPORT_LOCK, || f(&mut HostImportTransaction { _thread: PhantomData }))
}

thread_local! {
    static IN_IMPORT_OPERATION: Cell<bool> = const { Cell::new(false) };
}

struct ImportOperationGuard;

impl Drop for ImportOperationGuard {
    fn drop(&mut self) {
        IN_IMPORT_OPERATION.set(false);
    }
}

fn with_import_lock<T>(lock: &Mutex<()>, f: impl FnOnce() -> LeanResult<T>) -> LeanResult<T> {
    with_import_lock_timeout(lock, Duration::from_secs(30), f)
}

fn with_import_lock_timeout<T>(
    lock: &Mutex<()>,
    timeout: Duration,
    f: impl FnOnce() -> LeanResult<T>,
) -> LeanResult<T> {
    if IN_IMPORT_OPERATION.get() {
        return Err(crate::__host_internals::host_resource_exhausted(
            "recursive host import/source operation is not supported",
        ));
    }
    let started = Instant::now();
    let mut attempted = false;
    let _import_guard = loop {
        if attempted && started.elapsed() >= timeout {
            return Err(crate::__host_internals::host_resource_exhausted(
                "timed out waiting for another host import/source operation in this process",
            ));
        }
        attempted = true;
        match lock.try_lock() {
            Ok(guard) => break guard,
            Err(TryLockError::Poisoned(poisoned)) => break poisoned.into_inner(),
            Err(TryLockError::WouldBlock) => {
                let remaining = timeout.saturating_sub(started.elapsed());
                if remaining.is_zero() {
                    return Err(crate::__host_internals::host_resource_exhausted(
                        "timed out waiting for another host import/source operation in this process",
                    ));
                }
                std::thread::sleep(remaining.min(Duration::from_millis(10)));
            }
        }
    };
    IN_IMPORT_OPERATION.set(true);
    let _operation_guard = ImportOperationGuard;
    f()
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    reason = "test assertions fail the test on unexpected lock behavior"
)]
mod import_lock_tests {
    use super::{with_import_lock, with_import_lock_timeout};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    #[test]
    fn import_lock_reentry_fails_without_waiting() {
        let lock = Mutex::new(());
        let error = with_import_lock(&lock, || with_import_lock(&lock, || Ok(()))).expect_err("reentry fails");
        assert!(error.to_string().contains("recursive"));
        with_import_lock(&lock, || Ok(())).expect("outer guard released");
    }

    #[test]
    #[allow(clippy::panic, reason = "exercise guard release during Rust unwinding")]
    fn import_lock_panic_releases_guard() {
        let lock = Mutex::new(());
        let panic = std::panic::catch_unwind(|| with_import_lock::<()>(&lock, || panic!("test panic")));
        assert!(panic.is_err());
        with_import_lock(&lock, || Ok(())).expect("poisoned guard is recoverable");
    }

    #[test]
    fn import_lock_concurrent_call_waits_then_succeeds() {
        let lock = Arc::new(Mutex::new(()));
        let guard = lock.lock().expect("hold lock");
        let other = Arc::clone(&lock);
        let (entered, received) = std::sync::mpsc::channel();
        let waiter = std::thread::spawn(move || {
            with_import_lock(&other, || {
                entered.send(()).expect("report entry");
                Ok(())
            })
        });
        assert!(received.recv_timeout(Duration::from_millis(50)).is_err());
        drop(guard);
        received
            .recv_timeout(Duration::from_secs(5))
            .expect("waiter enters after release");
        waiter.join().expect("join").expect("concurrent operation succeeds");
    }

    #[test]
    #[allow(clippy::panic, reason = "assert the guarded body never runs on timeout")]
    fn import_lock_timeout_does_not_enter_or_poison_next_call() {
        let lock = Arc::new(Mutex::new(()));
        let guard = lock.lock().expect("hold lock");
        let other = Arc::clone(&lock);
        let waiter = std::thread::spawn(move || {
            let started = Instant::now();
            let error = with_import_lock_timeout::<()>(&other, Duration::from_millis(20), || {
                panic!("must not enter while another operation owns the lock")
            })
            .expect_err("bounded contention returns an error");
            assert!(error.to_string().contains("timed out"));
            assert!(started.elapsed() >= Duration::from_millis(20));
        });
        waiter.join().expect("join");
        drop(guard);
        with_import_lock(&lock, || Ok(())).expect("capacity available again");
    }
}
