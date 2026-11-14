//! Process ownership and confirmed-exit accounting, including failed startup.
use std::io;
use std::ops::{Deref, DerefMut};
use std::process::{Child, ExitStatus};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Observe all child generations created with one worker configuration.
///
/// Counts decrease only after `wait` or `try_wait` confirms exit. After its owner
/// stops, an embedder may release capacity when this count reaches zero. A zero
/// count while the owner is still running does not prevent a later spawn.
/// Keep and poll this tracker until zero if cleanup-thread creation fails;
/// dropping the last tracker in that case can discard pending process handles.
#[derive(Clone, Debug, Default)]
pub struct LeanWorkerProcessTracker(Arc<Processes>);

#[derive(Debug, Default)]
struct Processes {
    active: AtomicUsize,
    pending: Mutex<Vec<Child>>,
    reaper_running: AtomicBool,
}

impl LeanWorkerProcessTracker {
    /// Create an empty tracker shared by a worker and all of its replacements.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of children whose exit has not been confirmed.
    ///
    /// Also retries cleanup left by dropped owners, including after a failed
    /// attempt to start the cleanup thread. This never infers death from a PID.
    #[must_use]
    pub fn active_processes(&self) -> usize {
        self.reap_pending();
        self.start_reaper();
        self.0.active.load(Ordering::Acquire)
    }

    pub(crate) fn own(&self, child: Child) -> OwnedChild {
        self.0.active.fetch_add(1, Ordering::AcqRel);
        OwnedChild {
            child: Some(child),
            tracker: self.clone(),
            confirmed: false,
        }
    }

    fn reap_pending(&self) {
        let mut pending = self.0.pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        pending.retain_mut(|child| match child.try_wait() {
            Ok(Some(_)) => {
                self.0.active.fetch_sub(1, Ordering::AcqRel);
                false
            }
            Ok(None) => {
                // Only our still-unreaped child may receive a kill. In
                // particular, an ECHILD error must not target a reused PID.
                let _kill = child.kill();
                true
            }
            Err(_) => true,
        });
    }

    fn start_reaper(&self) {
        if self
            .0
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_empty()
            || self.0.reaper_running.swap(true, Ordering::AcqRel)
        {
            return;
        }
        let tracker = self.clone();
        let result = std::thread::Builder::new()
            .name("lean-worker-reaper".into())
            .spawn(move || {
                loop {
                    tracker.reap_pending();
                    let pending = tracker
                        .0
                        .pending
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    if pending.is_empty() {
                        // Clear while holding the registry lock, so a concurrent
                        // retirement either sees this thread or starts the next one.
                        tracker.0.reaper_running.store(false, Ordering::Release);
                        break;
                    }
                    drop(pending);
                    std::thread::sleep(Duration::from_millis(100));
                }
            });
        if result.is_err() {
            // The tracker still owns every pending Child. Its next poll or
            // retirement retries; no capacity is released on this error.
            self.0.reaper_running.store(false, Ordering::Release);
        }
    }
}

#[derive(Debug)]
pub(crate) struct OwnedChild {
    child: Option<Child>,
    tracker: LeanWorkerProcessTracker,
    confirmed: bool,
}

impl OwnedChild {
    pub(crate) fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        let result = self.deref_mut().try_wait()?;
        if result.is_some() && !self.confirmed {
            self.confirmed = true;
            self.tracker.0.active.fetch_sub(1, Ordering::AcqRel);
        }
        Ok(result)
    }
}

#[allow(
    clippy::expect_used,
    reason = "child is Some until Drop takes it; Deref is never called afterward"
)]
impl Deref for OwnedChild {
    type Target = Child;
    fn deref(&self) -> &Child {
        // The handle moves out only inside Drop, after all other methods finish.
        self.child.as_ref().expect("owned child is present outside Drop")
    }
}
#[allow(
    clippy::expect_used,
    reason = "child is Some until Drop takes it; DerefMut is never called afterward"
)]
impl DerefMut for OwnedChild {
    fn deref_mut(&mut self) -> &mut Child {
        // The handle moves out only inside Drop, after all other methods finish.
        self.child.as_mut().expect("owned child is present outside Drop")
    }
}
impl Drop for OwnedChild {
    fn drop(&mut self) {
        if self.confirmed {
            return;
        }
        if matches!(self.try_wait(), Ok(Some(_))) {
            return;
        }
        if let Some(child) = self.child.take() {
            self.tracker
                .0
                .pending
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(child);
            self.tracker.start_reaper();
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn sleeper() -> Child {
        std::process::Command::new("sleep")
            .arg("60")
            .spawn()
            .expect("spawn test child")
    }

    #[test]
    fn live_owner_is_not_reaped_by_observation() {
        let tracker = LeanWorkerProcessTracker::new();
        let mut owned = tracker.own(sleeper());
        assert_eq!(tracker.active_processes(), 1);
        assert!(owned.try_wait().expect("poll child").is_none());
        owned.kill().expect("kill child");
        while owned.try_wait().expect("reap child").is_none() {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(tracker.active_processes(), 0);
        drop(owned);
        assert_eq!(tracker.active_processes(), 0);
    }

    #[test]
    fn shared_tracker_retains_both_children_through_unwind_and_concurrent_drop() {
        let tracker = LeanWorkerProcessTracker::new();
        let first = tracker.own(sleeper());
        let second = tracker.own(sleeper());
        assert_eq!(tracker.active_processes(), 2);
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let peer = Arc::clone(&barrier);
        let unwind = std::thread::spawn(move || {
            let _owned = first;
            peer.wait();
            panic!("injected owner panic");
        });
        let retirement = std::thread::spawn(move || {
            barrier.wait();
            drop(second);
        });
        assert!(unwind.join().is_err());
        retirement.join().expect("second owner stopped");
        let started = Instant::now();
        while tracker.0.active.load(Ordering::Acquire) != 0 {
            assert!(
                started.elapsed() < Duration::from_secs(3),
                "shared reaper did not finish"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(tracker.active_processes(), 0);
    }

    #[test]
    fn dropped_child_is_reaped_without_touching_another_child() {
        let tracker = LeanWorkerProcessTracker::new();
        let mut unrelated = sleeper();
        let owned = tracker.own(sleeper());
        assert_eq!(tracker.active_processes(), 1);
        drop(owned);
        let started = Instant::now();
        while tracker.0.active.load(Ordering::Acquire) != 0 {
            assert!(started.elapsed() < Duration::from_secs(3), "reaper did not finish");
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(unrelated.try_wait().expect("poll unrelated").is_none());
        unrelated.kill().expect("kill unrelated test child");
        unrelated.wait().expect("reap unrelated test child");
    }
}
