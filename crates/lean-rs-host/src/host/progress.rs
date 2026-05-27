//! Structured progress reporting for host-session operations.
//!
//! Progress is host policy layered over the `lean-rs` scoped progress
//! callback. Public callers provide a borrowed [`LeanProgressSink`]; this
//! module maps Lean progress ticks into host phases and elapsed time.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::time::{Duration, Instant};

use lean_rs::abi::traits::TryFromLean;
use lean_rs::{LeanCallbackFlow, LeanProgressCallback, LeanResult, Obj};

/// One structured progress observation from a `LeanSession` operation.
///
/// Events are delivered synchronously on the Lean-bound worker thread.
/// `current` is phase-local and monotonically non-decreasing within one
/// `phase`. `total = None` means the operation can report a phase-local
/// counter but does not know the final bound cheaply.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LeanProgressEvent {
    /// Stable phase label for the current operation.
    pub phase: &'static str,
    /// Phase-local progress counter.
    pub current: u64,
    /// Optional phase-local total.
    pub total: Option<u64>,
    /// Time elapsed since this phase started.
    pub elapsed: Duration,
}

/// Sink for structured host progress events.
///
/// A sink is a one-way callback. It runs synchronously on the thread
/// currently executing the `LeanSession` method and must not call back
/// into the same session or re-enter the same Lean call stack. Expensive
/// work should be queued elsewhere by the sink because progress may be
/// reported many times for bulk operations.
///
/// Rust panics from sinks invoked through Lean progress shims are
/// contained by the `lean-rs` callback trampoline and returned as a host
/// internal error. Panics from host-side progress checkpoints are caught
/// before they escape the session method.
pub trait LeanProgressSink: Send + Sync {
    /// Receive one progress event.
    fn report(&self, event: LeanProgressEvent);
}

pub(crate) struct ProgressBridge<'a> {
    callback: LeanProgressCallback<'a>,
}

impl<'a> ProgressBridge<'a> {
    pub(crate) fn new(sink: &'a dyn LeanProgressSink, phase: &'static str, total: Option<u64>) -> LeanResult<Self> {
        let started = Instant::now();
        let callback = LeanProgressCallback::register(move |event| {
            sink.report(LeanProgressEvent {
                phase,
                current: event.current,
                total,
                elapsed: started.elapsed(),
            });
            LeanCallbackFlow::Continue
        })?;
        Ok(Self { callback })
    }

    pub(crate) fn abi_parts(&self) -> (usize, usize) {
        self.callback.abi_parts()
    }

    pub(crate) fn decode<'lean, T>(&self, obj: Obj<'lean>) -> LeanResult<T>
    where
        T: TryFromLean<'lean>,
    {
        self.callback.decode_result(obj)
    }
}

pub(crate) fn report_progress(
    sink: Option<&dyn LeanProgressSink>,
    phase: &'static str,
    current: u64,
    total: Option<u64>,
    started: Instant,
) -> LeanResult<()> {
    let Some(sink) = sink else {
        return Ok(());
    };
    let event = LeanProgressEvent {
        phase,
        current,
        total,
        elapsed: started.elapsed(),
    };
    catch_unwind(AssertUnwindSafe(|| sink.report(event)))
        .map_err(|payload| lean_rs::__host_internals::host_callback_panic(payload.as_ref()))
}
