//! Process ownership and confirmed-exit accounting, including failed startup.
use std::io;
use std::process::{Child, ChildStderr, ChildStdin, ChildStdout, ExitStatus};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Observe all child generations created with one worker configuration.
///
/// Counts decrease after `wait` or `try_wait` confirms exit and inherited-group
/// closure. After ECHILD revokes wait authority, an attached lifetime's passive whole-group absence
/// may release resource capacity without manufacturing an exit-status receipt.
/// After its owner stops, an embedder may release capacity when this reaches zero. A zero
/// count while the owner is still running does not prevent a later spawn.
/// Cleanup ownership is established before spawn and survives loss of the last client.
#[derive(Clone, Debug, Default)]
pub struct LeanWorkerProcessTracker(Arc<Processes>, Option<Arc<ProcessAttempt>>);

#[derive(Debug, Default)]
struct ProcessAttempt {
    active: AtomicUsize,
}

/// Exact process-scope accounting for one caller-defined launch attempt.
///
/// The observation is additional to the caller's tracker. It does not own or
/// signal a process and becomes closed only after every launch made through
/// its derived tracker has released its owned resource scope.
#[derive(Debug)]
pub(crate) struct LeanWorkerProcessAttempt(Arc<ProcessAttempt>);

impl LeanWorkerProcessAttempt {
    pub(crate) fn closed(&self) -> bool {
        self.0.active.load(Ordering::Acquire) == 0
    }
}

#[derive(Debug, Default)]
struct Processes {
    active: AtomicUsize,
    pending: Mutex<Vec<PendingChild>>,
    reaper: Mutex<Option<std::thread::Thread>>,
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

#[cfg(unix)]
fn custody_closed(
    lifetime: Option<&crate::crash_ownership::ParentLifetimeLease>,
    bootstrap: Option<&crate::crash_ownership::BootstrapCustody>,
) -> bool {
    bootstrap.is_none_or(|b| matches!(b.closed(), Ok(true)))
        && lifetime.is_none_or(|l| matches!(l.retirement_closed(), Ok(true)))
}

#[cfg(unix)]
fn custody_closed_after_direct_reap(
    lifetime: Option<&crate::crash_ownership::ParentLifetimeLease>,
    bootstrap: Option<&crate::crash_ownership::BootstrapCustody>,
) -> bool {
    bootstrap.is_none_or(|b| matches!(b.closed_after_direct_reap(), Ok(true)))
        && lifetime.is_none_or(|l| matches!(l.retirement_closed(), Ok(true)))
}

#[derive(Debug)]
struct PendingChild {
    child: OwnedProcess,
    exit: LeanWorkerExitObservation,
    attempt: Option<Arc<ProcessAttempt>>,
    #[cfg(unix)]
    lifetime: Option<crate::crash_ownership::ParentLifetimeLease>,
    #[cfg(unix)]
    bootstrap: Option<crate::crash_ownership::BootstrapCustody>,
}

#[derive(Debug)]
pub(crate) enum CheckedSpawnError<E> {
    Io(io::Error),
    Refused {
        reason: E,
        retirement_error: Option<io::Error>,
    },
}

impl<E> From<io::Error> for CheckedSpawnError<E> {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

#[cfg(unix)]
impl CheckedSpawnError<std::convert::Infallible> {
    fn into_io(self) -> io::Error {
        match self {
            Self::Io(error) => error,
            Self::Refused { reason, .. } => match reason {},
        }
    }
}

impl LeanWorkerProcessTracker {
    /// Create an empty tracker shared by a worker and all of its replacements.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Derive a tracker which preserves this ownership domain while also
    /// recording only launches made through the derived value.
    pub(crate) fn observe_attempt(&self) -> (Self, LeanWorkerProcessAttempt) {
        let attempt = Arc::new(ProcessAttempt::default());
        (
            Self(Arc::clone(&self.0), Some(Arc::clone(&attempt))),
            LeanWorkerProcessAttempt(attempt),
        )
    }

    /// Number of reserved launches and children whose owned resource scope has not closed.
    ///
    /// Also polls cleanup left by dropped owners. This never infers death from a PID.
    #[must_use]
    pub fn active_processes(&self) -> usize {
        self.reap_pending();
        self.0.active.load(Ordering::Acquire)
    }

    pub(crate) fn spawn(&self, command: &mut std::process::Command) -> io::Result<OwnedChild> {
        self.launch(Self::start_reaper, || command.spawn())
    }

    pub(crate) fn spawn_checked<E>(
        &self,
        command: &mut std::process::Command,
        admit: impl FnOnce() -> Result<(), E>,
    ) -> Result<OwnedChild, CheckedSpawnError<E>> {
        self.launch_with_error(Self::start_reaper, || {
            admit().map_err(|reason| CheckedSpawnError::Refused {
                reason,
                retirement_error: None,
            })?;
            command.spawn().map_err(CheckedSpawnError::Io)
        })
    }

    #[cfg(unix)]
    pub(crate) fn spawn_guarded(
        &self,
        command: &mut std::process::Command,
        lifetime: &mut crate::crash_ownership::ParentLifetime,
    ) -> io::Result<OwnedChild> {
        self.spawn_guarded_checked(command, lifetime, || Ok(()))
            .map_err(CheckedSpawnError::into_io)
    }

    #[cfg(unix)]
    pub(crate) fn spawn_guarded_checked<E>(
        &self,
        command: &mut std::process::Command,
        lifetime: &mut crate::crash_ownership::ParentLifetime,
        admit: impl FnOnce() -> Result<(), E>,
    ) -> Result<OwnedChild, CheckedSpawnError<E>> {
        let custody = lifetime.custody.clone();
        let mut child = self.launch_with_error(Self::start_reaper, || {
            lifetime.dispatch()?;
            if let Err(reason) = admit() {
                // This closure is the sole launch owner and has not called
                // spawn. If retirement fails, keep the record for the normal collector.
                let retirement_error = lifetime.no_spawn().err();
                return Err(CheckedSpawnError::Refused {
                    reason,
                    retirement_error,
                });
            }
            match command.spawn() {
                Ok(child) => Ok(child),
                Err(error) => {
                    let _retired = lifetime.no_spawn();
                    Err(CheckedSpawnError::Io(error))
                }
            }
        })?;
        child.bootstrap = Some(custody);
        Ok(child)
    }

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
            let mut reaper = self.0.reaper.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            if reaper.is_none() {
                *reaper = Some(start(self.clone())?);
            }
            self.0.active.fetch_add(1, Ordering::AcqRel);
            if let Some(attempt) = &self.1 {
                attempt.active.fetch_add(1, Ordering::AcqRel);
            }
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
            child: Some(OwnedProcess::Direct(child)),
            tracker: self.clone(),
            confirmed: false,
            cleanup_requested: false,
            direct_exit_observed: false,
            exit: LeanWorkerExitObservation::default(),
            #[cfg(unix)]
            lifetime: None,
            #[cfg(unix)]
            bootstrap: None,
        })
    }

    fn reap_pending(&self) {
        let mut pending = self.0.pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        pending.retain_mut(|owned| {
            #[cfg(unix)]
            if let Some(b) = &owned.bootstrap {
                b.request_stop();
            }
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
                    if !custody_closed_after_direct_reap(owned.lifetime.as_ref(), owned.bootstrap.as_ref()) {
                        return true;
                    }
                    owned.exit.confirm(status);
                    #[cfg(unix)]
                    if let Some(lifetime) = &owned.lifetime {
                        let _removed = lifetime.remove_if_absent();
                    }
                    self.release_for(owned.attempt.as_ref());
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
                        && let Some(lifetime) = &owned.lifetime
                    {
                        let _stop = lifetime.request_stop();
                    }
                    #[cfg(unix)]
                    if error.raw_os_error() == Some(libc::ECHILD)
                        && (owned.bootstrap.is_some() || owned.lifetime.is_some())
                        && custody_closed(owned.lifetime.as_ref(), owned.bootstrap.as_ref())
                    {
                        // No wait status exists: release physical capacity only.
                        // Serialized identities never authorize a recovery signal.
                        owned.exit.confirm_scope_closed_without_status();
                        self.release_for(owned.attempt.as_ref());
                        return false;
                    }
                    #[cfg(not(unix))]
                    drop(error);
                    true
                }
            }
        });
    }

    fn wake(&self) {
        if let Some(thread) = self
            .0
            .reaper
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
        {
            thread.unpark();
        }
    }

    fn release(&self) {
        self.release_for(self.1.as_ref());
    }

    fn release_for(&self, attempt: Option<&Arc<ProcessAttempt>>) {
        if let Some(attempt) = attempt {
            attempt.active.fetch_sub(1, Ordering::AcqRel);
        }
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
                        let mut reaper = tracker
                            .0
                            .reaper
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
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
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
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
    pub(crate) lifetime: Option<crate::crash_ownership::ParentLifetimeLease>,
    #[cfg(unix)]
    bootstrap: Option<crate::crash_ownership::BootstrapCustody>,
}

impl OwnedChild {
    #[cfg(test)]
    pub(crate) fn test_active_processes(&self) -> usize {
        self.tracker.active_processes()
    }

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
        self.lifetime
            .as_mut()
            .ok_or_else(|| io::Error::other("lifetime missing"))?
            .publish_running()?;
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

    #[cfg(unix)]
    #[allow(dead_code, reason = "internal managed adapter pending its O3 caller")]
    pub(crate) fn arm_managed_request(
        &mut self,
        arm: &crate::crash_ownership::ManagedRequestDeadline,
    ) -> io::Result<()> {
        self.lifetime
            .as_mut()
            .ok_or_else(|| io::Error::other("managed worker lifetime lease missing"))?
            .arm_managed_request(arm)
    }

    pub(crate) fn kill(&mut self) -> io::Result<()> {
        self.cleanup_requested = true;
        #[cfg(unix)]
        if let Some(b) = &self.bootstrap {
            b.request_stop();
        }
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
                    && let Some(lifetime) = &self.lifetime
                {
                    let _stop = lifetime.request_stop();
                }
                #[cfg(unix)]
                if error.raw_os_error() == Some(libc::ECHILD)
                    && !self.confirmed
                    && (self.bootstrap.is_some() || self.lifetime.is_some())
                    && custody_closed(self.lifetime.as_ref(), self.bootstrap.as_ref())
                {
                    self.confirmed = true;
                    self.exit.confirm_scope_closed_without_status();
                    self.tracker.release();
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
            if let Some(b) = &self.bootstrap {
                b.request_stop();
            }
            #[cfg(unix)]
            if let Some(lifetime) = &self.lifetime {
                let _stop = lifetime.request_stop();
            }
            #[cfg(unix)]
            if !custody_closed_after_direct_reap(self.lifetime.as_ref(), self.bootstrap.as_ref()) {
                return Ok(None);
            }
            if let Some(status) = result {
                self.exit.confirm(status);
            }
            self.confirmed = true;
            self.tracker.release();
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
                    attempt: self.tracker.1.clone(),
                    #[cfg(unix)]
                    lifetime: self.lifetime.take(),
                    #[cfg(unix)]
                    bootstrap: self.bootstrap.take(),
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

    #[cfg(unix)]
    #[test]
    fn all_release_paths_share_unknown_bootstrap_fence() {
        let bootstrap = crate::crash_ownership::BootstrapCustody::default();
        assert!(!custody_closed(None, Some(&bootstrap)));
        assert!(!custody_closed_after_direct_reap(None, Some(&bootstrap)));
        let deferred = bootstrap.clone();
        drop(bootstrap);
        assert!(!custody_closed(None, Some(&deferred)));
        assert!(custody_closed(None, None)); // Only genuinely unguarded direct children.
    }

    #[cfg(unix)]
    #[test]
    fn ordinary_pre_auth_requires_direct_reap_not_unknown_wait() {
        let bootstrap = crate::crash_ownership::BootstrapCustody::for_spawn(false);
        let deferred = bootstrap.clone();
        assert!(!custody_closed(None, Some(&bootstrap)));
        assert!(custody_closed_after_direct_reap(None, Some(&bootstrap)));
        // The background reaper uses the same evidence distinction.
        assert!(!custody_closed(None, Some(&deferred)));
        assert!(custody_closed_after_direct_reap(None, Some(&deferred)));
    }

    impl LeanWorkerProcessTracker {
        pub(crate) fn own(&self, child: Child) -> OwnedChild {
            self.launch(Self::start_reaper, || Ok(child))
                .expect("test cleanup thread starts")
        }
    }

    fn sleeper() -> Child {
        std::process::Command::new("sleep")
            .arg("60")
            .spawn()
            .expect("spawn test child")
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
        let first = tracker
            .spawn(std::process::Command::new("sleep").arg("60"))
            .expect("launch");
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
        let owned = tracker
            .spawn(std::process::Command::new("sleep").arg("60"))
            .expect("launch");
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
