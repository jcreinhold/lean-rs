//! Cooperative cancellation for long-running session operations.
//!
//! The token is deliberately just one atomic bit. Policy stays with the
//! caller: UI code, request handlers, or worker supervisors decide when to
//! flip the bit; `lean-rs-host` only checks it at documented boundaries.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use lean_rs::LeanResult;

/// Shared cooperative cancellation token for `LeanSession` calls.
///
/// `LeanCancellationToken` is `Clone + Send + Sync`, so a coordinator
/// thread can keep one clone and the Lean-bound worker thread can pass
/// another clone by reference to session methods. Cancelling sets one
/// atomic bit. Session methods check that bit before FFI dispatches and,
/// for token-aware bulk paths, between per-item dispatches.
///
/// This is not pre-emptive cancellation. It does **not** interrupt an
/// in-flight Lean call. In particular, it cannot stop one stuck `isDefEq`,
/// a C-side kernel reduction that does not check heartbeats, or an
/// `@[export]` symbol that runs its own long loop without returning to a
/// `lean-rs-host` check point. Bound those workloads with Lean heartbeat
/// options, caller-level timeouts, or a worker-process boundary.
///
/// ```no_run
/// use std::thread;
/// use lean_rs_host::LeanCancellationToken;
///
/// let token = LeanCancellationToken::new();
/// let canceller = token.clone();
///
/// thread::spawn(move || {
///     // A UI event, request timeout, or supervisor decision would live here.
///     canceller.cancel();
/// });
///
/// // On the Lean worker thread:
/// // session.query_declarations_bulk(&names, Some(&token))?;
/// # let _ = token;
/// ```
#[derive(Clone, Debug, Default)]
pub struct LeanCancellationToken {
    cancelled: Arc<AtomicBool>,
}

impl LeanCancellationToken {
    /// Create a token in the non-cancelled state.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Mark this token as cancelled.
    ///
    /// Cancellation is one-way. Create a fresh token for a later operation.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    /// Whether cancellation has been requested.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }
}

pub(crate) fn check_cancellation(token: Option<&LeanCancellationToken>) -> LeanResult<()> {
    if token.is_some_and(LeanCancellationToken::is_cancelled) {
        return Err(lean_rs::__host_internals::host_cancelled());
    }
    Ok(())
}
