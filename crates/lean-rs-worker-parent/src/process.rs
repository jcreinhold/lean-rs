//! Process ownership, confirmed-exit accounting and the parent lifeline.
use std::io;
use std::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, ExitStatus};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

/// Names the inherited read end of a worker's parent lifeline pipe.
const LIFELINE_ENV: &str = "LEAN_RS_WORKER_PARENT_LIFELINE";

/// Observe all child generations created with one worker configuration.
///
/// Counts decrease after `wait` or `try_wait` confirms exit. After its owner
/// stops, an embedder may release capacity when this reaches zero. A zero
/// count while the owner is still running does not prevent a later spawn.
/// Cleanup ownership is established before spawn and survives loss of the last client.
#[derive(Clone, Debug, Default)]
pub struct LeanWorkerProcessTracker(Arc<Processes>);

#[derive(Debug, Default)]
struct Processes {
    active: AtomicUsize,
    pending: Mutex<Vec<PendingChild>>,
    reaper: Mutex<Option<std::thread::Thread>>,
}

/// Shared exit evidence for one owned direct child, never a process signal handle.
///
/// An unresolved observation does not prove the child is running. Successful
/// waiting records its actual status.
#[derive(Clone, Debug, Default)]
pub struct LeanWorkerExitObservation(Arc<Mutex<Option<ExitStatus>>>);

impl LeanWorkerExitObservation {
    /// Return the exit status confirmed by waiting on this exact owned child.
    #[must_use]
    pub fn confirmed_exit(&self) -> Option<ExitStatus> {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn confirm(&self, status: ExitStatus) {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get_or_insert(status);
    }
}

#[derive(Debug)]
struct PendingChild {
    child: Child,
    exit: LeanWorkerExitObservation,
}

#[derive(Debug)]
pub(crate) enum CheckedSpawnError<E> {
    Io(io::Error),
    Refused(E),
}

impl<E> From<io::Error> for CheckedSpawnError<E> {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// Serializes lifeline creation with worker spawns. On platforms without an
/// atomic close-on-exec pipe, a worker that forks between another worker's
/// `pipe` and `fcntl` calls would otherwise inherit that worker's write end.
#[cfg(unix)]
static LIFELINE_SPAWN: Mutex<()> = Mutex::new(());

/// Spawn `command` with the read end of a fresh lifeline pipe and return the
/// write end, which only this process holds.
///
/// The child watches the read end and exits when it reaches end of file,
/// which happens when this process closes the write end or dies.
#[cfg(unix)]
fn spawn_with_lifeline(mut command: Command) -> io::Result<(Child, Option<io::PipeWriter>)> {
    use std::os::fd::{AsFd as _, AsRawFd as _};
    let _serial = LIFELINE_SPAWN.lock().unwrap_or_else(PoisonError::into_inner);
    let (reader, writer) = io::pipe()?;
    command.env(LIFELINE_ENV, reader.as_raw_fd().to_string());
    let child = lean_toolchain::process_group::spawn_inheriting(command, &[reader.as_fd()])?;
    Ok((child, Some(writer)))
}

#[cfg(not(unix))]
fn spawn_with_lifeline(mut command: Command) -> io::Result<(Child, Option<io::PipeWriter>)> {
    command.env_remove(LIFELINE_ENV);
    Ok((command.spawn()?, None))
}

impl LeanWorkerProcessTracker {
    /// Create an empty tracker shared by a worker and all of its replacements.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of reserved launches and children whose exit is not yet confirmed.
    ///
    /// Also polls cleanup left by dropped owners. This never infers death from a PID.
    #[must_use]
    pub fn active_processes(&self) -> usize {
        self.reap_pending();
        self.0.active.load(Ordering::Acquire)
    }

    /// Spawn a worker child that holds the read end of a parent lifeline.
    pub(crate) fn spawn(&self, command: Command) -> io::Result<OwnedChild> {
        self.spawn_checked(command, || Ok::<(), io::Error>(()))
            .map_err(|error| match error {
                CheckedSpawnError::Io(error) | CheckedSpawnError::Refused(error) => error,
            })
    }

    /// Spawn like [`Self::spawn`] once `admit` accepts the launch.
    pub(crate) fn spawn_checked<E>(
        &self,
        command: Command,
        admit: impl FnOnce() -> Result<(), E>,
    ) -> Result<OwnedChild, CheckedSpawnError<E>> {
        let mut lifeline = None;
        let mut child = self.launch_with_error(Self::start_reaper, || -> Result<Child, CheckedSpawnError<E>> {
            admit().map_err(CheckedSpawnError::Refused)?;
            let (child, writer) = spawn_with_lifeline(command)?;
            lifeline = writer;
            Ok(child)
        })?;
        child.lifeline = lifeline;
        Ok(child)
    }

    #[cfg(test)]
    fn launch(
        &self,
        start: impl FnOnce(Self) -> io::Result<std::thread::Thread>,
        spawn: impl FnOnce() -> io::Result<Child>,
    ) -> io::Result<OwnedChild> {
        self.launch_with_error(start, spawn)
    }

    fn launch_with_error<E: From<io::Error>>(
        &self,
        start: impl FnOnce(Self) -> io::Result<std::thread::Thread>,
        spawn: impl FnOnce() -> Result<Child, E>,
    ) -> Result<OwnedChild, E> {
        {
            let mut reaper = self.0.reaper.lock().unwrap_or_else(PoisonError::into_inner);
            if reaper.is_none() {
                *reaper = Some(start(self.clone())?);
            }
            self.0.active.fetch_add(1, Ordering::AcqRel);
            // Publish the reservation before allowing the reaper to decide to exit.
            drop(reaper);
        }
        let mut reservation = LaunchReservation(Some(self));
        let mut child = spawn()?;
        reservation.0 = None;
        Ok(OwnedChild {
            pid: child.id(),
            stdin: child.stdin.take(),
            stdout: child.stdout.take(),
            stderr: child.stderr.take(),
            child: Some(child),
            tracker: self.clone(),
            confirmed: false,
            cleanup_requested: false,
            exit: LeanWorkerExitObservation::default(),
            lifeline: None,
        })
    }

    fn reap_pending(&self) {
        let mut pending = self.0.pending.lock().unwrap_or_else(PoisonError::into_inner);
        pending.retain_mut(|owned| match owned.child.try_wait() {
            Ok(Some(status)) => {
                owned.exit.confirm(status);
                self.release();
                false
            }
            Ok(None) => {
                // Only our still-unreaped child may receive a kill. In
                // particular, an ECHILD error must not target a reused PID.
                let _kill = owned.child.kill();
                true
            }
            Err(_) => true,
        });
    }

    fn wake(&self) {
        if let Some(thread) = self.0.reaper.lock().unwrap_or_else(PoisonError::into_inner).as_ref() {
            thread.unpark();
        }
    }

    fn release(&self) {
        if self.0.active.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.wake();
        }
    }

    fn start_reaper(tracker: Self) -> io::Result<std::thread::Thread> {
        let handle = std::thread::Builder::new()
            .name("lean-worker-reaper".into())
            .spawn(move || {
                loop {
                    tracker.reap_pending();
                    {
                        let mut reaper = tracker.0.reaper.lock().unwrap_or_else(PoisonError::into_inner);
                        if tracker.0.active.load(Ordering::Acquire) == 0 {
                            *reaper = None;
                            drop(reaper);
                            return;
                        }
                        drop(reaper);
                    }
                    if tracker
                        .0
                        .pending
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .is_empty()
                    {
                        std::thread::park();
                    } else {
                        std::thread::park_timeout(Duration::from_millis(100));
                    }
                }
            })?;
        Ok(handle.thread().clone())
    }
}

struct LaunchReservation<'a>(Option<&'a LeanWorkerProcessTracker>);

impl Drop for LaunchReservation<'_> {
    fn drop(&mut self) {
        if let Some(tracker) = self.0 {
            tracker.release();
        }
    }
}

#[derive(Debug)]
pub(crate) struct OwnedChild {
    child: Option<Child>,
    pid: u32,
    pub(crate) stdin: Option<ChildStdin>,
    pub(crate) stdout: Option<ChildStdout>,
    pub(crate) stderr: Option<ChildStderr>,
    tracker: LeanWorkerProcessTracker,
    confirmed: bool,
    cleanup_requested: bool,
    exit: LeanWorkerExitObservation,
    /// The write end of the child's lifeline. Dropping it, as this value's
    /// drop does, makes the child exit.
    lifeline: Option<io::PipeWriter>,
}

impl OwnedChild {
    pub(crate) fn id(&self) -> u32 {
        self.pid
    }

    pub(crate) fn kill(&mut self) -> io::Result<()> {
        self.cleanup_requested = true;
        let child = self
            .child
            .as_mut()
            .ok_or_else(|| io::Error::other("owned child missing"))?;
        // A fresh None is required before signaling a raw Child: ECHILD
        // never permits a signal even if its numeric PID was reused.
        match child.try_wait()? {
            Some(_) => Ok(()),
            None => child.kill(),
        }
    }

    /// Nonpolling admission fact; never an exit or capacity-release witness.
    pub(crate) const fn cleanup_pending(&self) -> bool {
        !self.confirmed && self.cleanup_requested
    }

    pub(crate) fn exit_observation(&self) -> LeanWorkerExitObservation {
        self.exit.clone()
    }

    pub(crate) fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        let result = self
            .child
            .as_mut()
            .ok_or_else(|| io::Error::other("owned child missing"))?
            .try_wait()?;
        if let Some(status) = result
            && !self.confirmed
        {
            self.exit.confirm(status);
            self.confirmed = true;
            self.tracker.release();
        }
        Ok(result)
    }
}

impl Drop for OwnedChild {
    fn drop(&mut self) {
        if self.confirmed {
            return;
        }
        let _observed = self.try_wait();
        if self.confirmed {
            return;
        }
        if let Some(child) = self.child.take() {
            self.tracker
                .0
                .pending
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(PendingChild {
                    child,
                    exit: self.exit.clone(),
                });
            self.tracker.wake();
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use std::time::Instant;

    impl LeanWorkerProcessTracker {
        pub(crate) fn own(&self, child: Child) -> OwnedChild {
            self.launch(Self::start_reaper, || Ok(child))
                .expect("test cleanup thread starts")
        }
    }

    fn sleep_command() -> Command {
        let mut command = Command::new("sleep");
        command.arg("60");
        command
    }

    fn sleeper() -> Child {
        sleep_command().spawn().expect("spawn test child")
    }

    #[test]
    fn reaper_start_failure_prevents_launch_and_releases_no_unowned_capacity() {
        let tracker = LeanWorkerProcessTracker::new();
        let calls = std::cell::Cell::new(0);
        let result = tracker.launch(
            |_| Err(io::Error::other("thread unavailable")),
            || {
                calls.set(calls.get() + 1);
                Ok(sleeper())
            },
        );
        assert_eq!(
            result.expect_err("must reject before launch").to_string(),
            "thread unavailable"
        );
        assert_eq!(calls.get(), 0);
        assert_eq!(tracker.active_processes(), 0);
        assert!(tracker.0.reaper.lock().expect("lock").is_none());
    }

    #[test]
    fn failed_launch_holds_reservation_until_spawn_returns() {
        let tracker = LeanWorkerProcessTracker::new();
        let failed = tracker.launch(LeanWorkerProcessTracker::start_reaper, || {
            assert_eq!(tracker.active_processes(), 1);
            Err(io::Error::other("spawn failed"))
        });
        assert_eq!(failed.expect_err("launch failed").to_string(), "spawn failed");
        assert_eq!(tracker.active_processes(), 0);
    }

    #[test]
    fn reservation_survives_concurrent_retirement_of_previous_child() {
        let tracker = LeanWorkerProcessTracker::new();
        let first = tracker.spawn(sleep_command()).expect("launch");
        let first_exit = first.exit_observation();
        let second = tracker
            .launch(LeanWorkerProcessTracker::start_reaper, || {
                assert_eq!(tracker.active_processes(), 2);
                drop(first);
                let start = Instant::now();
                while first_exit.confirmed_exit().is_none() || tracker.0.active.load(Ordering::Acquire) != 1 {
                    assert!(start.elapsed() < Duration::from_secs(3));
                    std::thread::yield_now();
                }
                // No child handle is enqueued here: the outstanding launch must keep this thread alive.
                assert!(tracker.0.reaper.lock().expect("lock").is_some());
                Ok(sleeper())
            })
            .expect("second launch");
        let second_exit = second.exit_observation();
        drop(second);
        drop(tracker);
        let start = Instant::now();
        while second_exit.confirmed_exit().is_none() {
            assert!(start.elapsed() < Duration::from_secs(3));
            std::thread::yield_now();
        }
    }

    #[test]
    fn final_client_loss_reaps_without_external_tracker_poll() {
        let tracker = LeanWorkerProcessTracker::new();
        let owned = tracker.spawn(sleep_command()).expect("launch");
        let evidence = owned.exit_observation();
        assert_eq!(tracker.active_processes(), 1);
        drop(tracker);
        drop(owned);
        let start = Instant::now();
        while evidence.confirmed_exit().is_none() {
            assert!(start.elapsed() < Duration::from_secs(3));
            std::thread::yield_now();
        }
    }

    #[test]
    fn live_owner_is_not_reaped_by_observation() {
        let tracker = LeanWorkerProcessTracker::new();
        let mut owned = tracker.own(sleeper());
        assert_eq!(tracker.active_processes(), 1);
        assert!(owned.try_wait().expect("poll child").is_none());
        assert!(!owned.cleanup_pending());
        owned.kill().expect("kill child");
        assert!(owned.cleanup_pending());
        while owned.try_wait().expect("reap child").is_none() {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(tracker.active_processes(), 0);
        assert!(!owned.cleanup_pending());
        drop(owned);
        assert_eq!(tracker.active_processes(), 0);
    }

    #[test]
    fn late_reap_keeps_exact_exit_observation_after_owner_drops() {
        let tracker = LeanWorkerProcessTracker::new();
        let owned = tracker.own(sleeper());
        let observation = owned.exit_observation();
        assert!(observation.confirmed_exit().is_none());
        drop(owned);
        let started = Instant::now();
        while observation.confirmed_exit().is_none() {
            assert!(started.elapsed() < Duration::from_secs(3), "late exit evidence lost");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(!observation.confirmed_exit().expect("observed exit").success());
        assert_eq!(tracker.active_processes(), 0);
    }

    #[test]
    fn exit_observation_never_settles_a_different_live_child() {
        let tracker = LeanWorkerProcessTracker::new();
        let first = tracker.own(sleeper());
        let second = tracker.own(sleeper());
        let first_exit = first.exit_observation();
        let second_exit = second.exit_observation();
        drop(first);
        let started = Instant::now();
        while first_exit.confirmed_exit().is_none() {
            assert!(started.elapsed() < Duration::from_secs(3), "late exit evidence lost");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(second_exit.confirmed_exit().is_none());
        assert_eq!(tracker.active_processes(), 1);
        drop(second);
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
