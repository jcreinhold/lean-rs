//! Process ownership and confirmed-exit accounting, including failed startup.
use std::io;
use std::process::{Child, ChildStderr, ChildStdin, ChildStdout, ExitStatus};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Observe all child generations created with one worker configuration.
///
/// Counts decrease after `wait` or `try_wait` confirms exit and inherited-group
/// closure. After ECHILD revokes wait authority, an attached lifetime's passive whole-group absence
/// may release resource capacity without manufacturing an exit-status receipt.
/// After its owner stops, an embedder may release capacity when this reaches zero. A zero
/// count while the owner is still running does not prevent a later spawn.
/// Keep and poll this tracker until zero if cleanup-thread creation fails;
/// dropping the last tracker in that case can discard pending process handles.
#[derive(Clone, Debug, Default)]
pub struct LeanWorkerProcessTracker(Arc<Processes>);

#[derive(Debug, Default)]
struct Processes {
    active: AtomicUsize,
    pending: Mutex<Vec<PendingChild>>,
    reaper_running: AtomicBool,
}

/// Shared exit evidence for one owned direct child, never a process signal handle.
///
/// An unresolved observation does not prove the child is running. Successful
/// waiting records its actual status. ECHILD plus whole-group absence instead
/// records terminal resource closure without inventing an exit status.
#[derive(Clone, Debug, Default)]
pub struct LeanWorkerExitObservation(Arc<Mutex<ExitObservationState>>);

#[derive(Debug, Default)]
struct ExitObservationState {
    status: Option<ExitStatus>,
    scope_closed_without_status: bool,
}

impl LeanWorkerExitObservation {
    /// Return the exit status confirmed by waiting on this exact owned child.
    #[must_use]
    pub fn confirmed_exit(&self) -> Option<ExitStatus> {
        self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner).status
    }

    /// Whether this exact owned resource scope has terminally closed after
    /// ECHILD, while its actual exit status remains unavailable.
    ///
    /// A retained request may stop watching lifecycle recovery on this fact.
    /// Its proof outcome remains unknown; this is never a stop-status receipt.
    #[must_use]
    pub fn scope_closed_without_status(&self) -> bool {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .scope_closed_without_status
    }

    fn confirm(&self, status: ExitStatus) {
        let mut observed = self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if observed.status.is_none() && !observed.scope_closed_without_status {
            observed.status = Some(status);
        }
    }

    #[cfg(unix)]
    fn confirm_scope_closed_without_status(&self) {
        let mut observed = self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if observed.status.is_none() {
            observed.scope_closed_without_status = true;
        }
    }
}

#[derive(Debug)]
enum OwnedProcess {
    Direct(Child),
    #[cfg(unix)]
    Group(lean_toolchain::process_group::OwnedCommandGroup),
}
impl OwnedProcess {
    fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        match self {
            Self::Direct(child) => child.try_wait(),
            #[cfg(unix)]
            Self::Group(group) => group.try_wait(),
        }
    }
    fn kill(&mut self) -> io::Result<()> {
        match self {
            // A fresh None is required before signaling a raw Child: ECHILD
            // never permits a signal even if its numeric PID was reused.
            Self::Direct(child) => match child.try_wait()? {
                Some(_) => Ok(()),
                None => child.kill(),
            },
            #[cfg(unix)]
            Self::Group(group) => group.request_stop(),
        }
    }
    fn cleanup_pending(&self) -> bool {
        match self {
            Self::Direct(_) => false,
            #[cfg(unix)]
            Self::Group(group) => group.cleanup_pending(),
        }
    }
}

#[cfg(unix)]
fn guardian_failed(lifetime: Option<&crate::crash_ownership::ParentLifetimeLease>) -> bool {
    lifetime.is_some_and(|lifetime| lifetime.uses_guardian() && !matches!(lifetime.guardian_alive(), Ok(true)))
}

#[derive(Debug)]
struct PendingChild {
    child: OwnedProcess,
    exit: LeanWorkerExitObservation,
    #[cfg(unix)]
    lifetime: Option<crate::crash_ownership::ParentLifetimeLease>,
}

impl LeanWorkerProcessTracker {
    /// Create an empty tracker shared by a worker and all of its replacements.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of children whose owned resource scope has not closed.
    ///
    /// Also retries cleanup left by dropped owners, including after a failed
    /// attempt to start the cleanup thread. This never infers death from a PID.
    #[must_use]
    pub fn active_processes(&self) -> usize {
        self.reap_pending();
        self.start_reaper();
        self.0.active.load(Ordering::Acquire)
    }

    pub(crate) fn own(&self, mut child: Child) -> OwnedChild {
        self.0.active.fetch_add(1, Ordering::AcqRel);
        OwnedChild {
            pid: child.id(),
            stdin: child.stdin.take(),
            stdout: child.stdout.take(),
            stderr: child.stderr.take(),
            child: Some(OwnedProcess::Direct(child)),
            tracker: self.clone(),
            confirmed: false,
            cleanup_requested: false,
            direct_exit_observed: false,
            exit: LeanWorkerExitObservation::default(),
            #[cfg(unix)]
            lifetime: None,
        }
    }

    fn reap_pending(&self) {
        let mut pending = self.0.pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        pending.retain_mut(|owned| {
            #[cfg(unix)]
            if guardian_failed(owned.lifetime.as_ref()) {
                if let Some(lifetime) = &owned.lifetime {
                    let _stop = lifetime.request_stop();
                }
                let _kill = owned.child.kill();
            }
            match owned.child.try_wait() {
                Ok(Some(status)) => {
                    #[cfg(unix)]
                    if let Some(lifetime) = &owned.lifetime {
                        let _stop = lifetime.request_stop();
                    }
                    #[cfg(unix)]
                    if owned
                        .lifetime
                        .as_ref()
                        .is_some_and(|lifetime| !matches!(lifetime.group_absent(), Ok(true)))
                    {
                        return true;
                    }
                    owned.exit.confirm(status);
                    #[cfg(unix)]
                    if let Some(lifetime) = &owned.lifetime {
                        let _removed = lifetime.remove_if_absent();
                    }
                    self.0.active.fetch_sub(1, Ordering::AcqRel);
                    false
                }
                Ok(None) => {
                    // Only our still-unreaped child may receive a kill. In
                    // particular, an ECHILD error must not target a reused PID.
                    #[cfg(unix)]
                    let _kill = if let Some(lifetime) = &mut owned.lifetime {
                        let requested = lifetime.request_stop();
                        let direct = owned.child.kill();
                        requested.or(direct)
                    } else {
                        owned.child.kill()
                    };
                    #[cfg(not(unix))]
                    let _kill = owned.child.kill();
                    true
                }
                Err(error) => {
                    #[cfg(unix)]
                    if error.raw_os_error() == Some(libc::ECHILD)
                        && owned
                            .lifetime
                            .as_ref()
                            .is_some_and(|lifetime| matches!(lifetime.group_absent(), Ok(true)))
                    {
                        // No wait status exists: release physical capacity only.
                        // Serialized identities never authorize a recovery signal.
                        owned.exit.confirm_scope_closed_without_status();
                        self.0.active.fetch_sub(1, Ordering::AcqRel);
                        return false;
                    }
                    #[cfg(not(unix))]
                    drop(error);
                    true
                }
            }
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
    child: Option<OwnedProcess>,
    pid: u32,
    pub(crate) stdin: Option<ChildStdin>,
    pub(crate) stdout: Option<ChildStdout>,
    pub(crate) stderr: Option<ChildStderr>,
    tracker: LeanWorkerProcessTracker,
    confirmed: bool,
    cleanup_requested: bool,
    direct_exit_observed: bool,
    exit: LeanWorkerExitObservation,
    #[cfg(unix)]
    lifetime: Option<crate::crash_ownership::ParentLifetimeLease>,
}

impl OwnedChild {
    #[cfg(unix)]
    pub(crate) fn attach_lifetime(&mut self, lifetime: crate::crash_ownership::ParentLifetimeLease) -> io::Result<()> {
        if lifetime.uses_guardian() {
            let process = self
                .child
                .take()
                .ok_or_else(|| io::Error::other("owned child missing"))?;
            self.child = Some(match process {
                OwnedProcess::Direct(child) => {
                    match lean_toolchain::process_group::OwnedCommandGroup::adopt_private_session(
                        child,
                        &lifetime.owner,
                    ) {
                        Ok(group) => OwnedProcess::Group(group),
                        Err((error, child)) => {
                            self.child = Some(OwnedProcess::Direct(child));
                            self.lifetime = Some(lifetime);
                            return Err(error);
                        }
                    }
                }
                OwnedProcess::Group(group) => OwnedProcess::Group(group),
            });
        }
        self.lifetime = Some(lifetime);
        Ok(())
    }

    pub(crate) fn id(&self) -> u32 {
        self.pid
    }

    #[cfg(unix)]
    pub(crate) fn has_guardian(&self) -> bool {
        self.lifetime
            .as_ref()
            .is_some_and(crate::crash_ownership::ParentLifetimeLease::uses_guardian)
    }

    /// Discover guardian failure through exact kernel birth, then request the
    /// common owned-group stop while this parent's actual root remains pinned.
    #[cfg(unix)]
    pub(crate) fn poll_guardian(&mut self) -> bool {
        let failed = guardian_failed(self.lifetime.as_ref());
        if failed {
            self.cleanup_requested = true;
            if let Some(lifetime) = &self.lifetime {
                let _stop = lifetime.request_stop();
            }
            if let Some(child) = &mut self.child {
                let _kill = child.kill();
            }
        }
        failed
    }

    #[cfg(unix)]
    pub(crate) fn acknowledge_lifetime(&mut self) -> io::Result<()> {
        self.lifetime
            .as_mut()
            .ok_or_else(|| io::Error::other("worker lifetime lease missing"))?
            .acknowledge()
    }

    pub(crate) fn kill(&mut self) -> io::Result<()> {
        self.cleanup_requested = true;
        #[cfg(unix)]
        if let Some(lifetime) = &mut self.lifetime {
            let requested = lifetime.request_stop();
            let direct = self
                .child
                .as_mut()
                .ok_or_else(|| io::Error::other("owned child missing"))?
                .kill();
            return requested.or(direct);
        }
        self.child
            .as_mut()
            .ok_or_else(|| io::Error::other("owned child missing"))?
            .kill()
    }
    /// Nonpolling admission fact; never an exit or capacity-release witness.
    pub(crate) fn cleanup_pending(&self) -> bool {
        !self.confirmed
            && (self.cleanup_requested
                || self.direct_exit_observed
                || self.child.as_ref().is_some_and(OwnedProcess::cleanup_pending))
    }

    pub(crate) fn exit_observation(&self) -> LeanWorkerExitObservation {
        self.exit.clone()
    }

    pub(crate) fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        #[cfg(unix)]
        let _guardian_failed = self.poll_guardian();
        let result = match self
            .child
            .as_mut()
            .ok_or_else(|| io::Error::other("owned child missing"))?
            .try_wait()
        {
            Ok(result) => result,
            Err(error) => {
                #[cfg(unix)]
                if error.raw_os_error() == Some(libc::ECHILD)
                    && !self.confirmed
                    && self
                        .lifetime
                        .as_ref()
                        .is_some_and(|lifetime| matches!(lifetime.group_absent(), Ok(true)))
                {
                    self.confirmed = true;
                    self.exit.confirm_scope_closed_without_status();
                    self.tracker.0.active.fetch_sub(1, Ordering::AcqRel);
                }
                // A capacity witness is not an exit-status witness. Preserve the
                // actual wait failure and leave the request observation unknown.
                return Err(error);
            }
        };
        #[cfg(unix)]
        if result.is_none() && self.child.as_ref().is_some_and(OwnedProcess::cleanup_pending) {
            self.cleanup_requested = true;
            if let Some(lifetime) = &self.lifetime {
                let _stop = lifetime.request_stop();
            }
            if let Some(child) = &mut self.child {
                let _kill = child.kill();
            }
        }
        if result.is_some() && !self.confirmed {
            self.direct_exit_observed = true;
            #[cfg(unix)]
            if let Some(lifetime) = &self.lifetime {
                let _stop = lifetime.request_stop();
            }
            #[cfg(unix)]
            if self
                .lifetime
                .as_ref()
                .is_some_and(|lifetime| !matches!(lifetime.group_absent(), Ok(true)))
            {
                return Ok(None); // The owned service still has inherited-group descendants.
            }
            if let Some(status) = result {
                self.exit.confirm(status);
            }
            self.confirmed = true;
            self.tracker.0.active.fetch_sub(1, Ordering::AcqRel);
            #[cfg(unix)]
            if let Some(lifetime) = &self.lifetime {
                let _removed = lifetime.remove_if_absent();
            }
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
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(PendingChild {
                    child,
                    exit: self.exit.clone(),
                    #[cfg(unix)]
                    lifetime: self.lifetime.take(),
                });
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
    #[cfg(unix)]
    fn terminal_observation_is_monotonic_and_clone_isolated() {
        let status = std::process::Command::new("true").status().expect("fixture status");
        let unknown = LeanWorkerExitObservation::default();
        let clone = unknown.clone();
        let unrelated = LeanWorkerExitObservation::default();
        unknown.confirm_scope_closed_without_status();
        unknown.confirm(status);
        assert!(clone.scope_closed_without_status());
        assert!(clone.confirmed_exit().is_none());
        assert!(!unrelated.scope_closed_without_status());
        assert!(unrelated.confirmed_exit().is_none());
        unrelated.confirm(status);
        unrelated.confirm_scope_closed_without_status();
        assert_eq!(unrelated.confirmed_exit(), Some(status));
        assert!(!unrelated.scope_closed_without_status());
        assert!(clone.scope_closed_without_status());
        assert!(clone.confirmed_exit().is_none());
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
