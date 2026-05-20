//! Structured progress reporting for host-session operations.
//!
//! Progress is host policy layered over the L1 `lean-rs` callback
//! registry. Public callers provide a borrowed [`LeanProgressSink`];
//! this module hides the temporary callback handle, ABI status byte,
//! and panic/stale-handle mapping needed by Lean progress shims.

// SAFETY DOC: this module has two narrow unsafe operations: turning a
// synchronous callback handle back into its stack-owned context pointer,
// and reading the scalar-tail `UInt8` from an `Except UInt8 _` error
// constructor after validating the constructor tag. Each block carries
// the local invariant.
#![allow(unsafe_code)]

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::time::{Duration, Instant};

use lean_rs::abi::structure::{ctor_tag, take_ctor_objects};
use lean_rs::abi::traits::{TryFromLean, conversion_error};
use lean_rs::{LeanCallbackFlow, LeanCallbackHandle, LeanCallbackStatus, LeanProgressTick, LeanResult, Obj};

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
/// contained by the L1 callback trampoline and returned as a host
/// internal error. Panics from host-side progress checkpoints are caught
/// before they escape the session method.
pub trait LeanProgressSink: Send + Sync {
    /// Receive one progress event.
    fn report(&self, event: LeanProgressEvent);
}

pub(crate) struct ProgressBridge<'a> {
    handle: LeanCallbackHandle<LeanProgressTick>,
    #[allow(dead_code, reason = "keeps the callback context alive until after handle drop")]
    context: Box<ProgressContext<'a>>,
}

struct ProgressContext<'a> {
    sink: &'a dyn LeanProgressSink,
    phase: &'static str,
    started: Instant,
    total: Option<u64>,
}

impl<'a> ProgressBridge<'a> {
    pub(crate) fn new(sink: &'a dyn LeanProgressSink, phase: &'static str, total: Option<u64>) -> LeanResult<Self> {
        let context = Box::new(ProgressContext {
            sink,
            phase,
            started: Instant::now(),
            total,
        });
        let context_ptr: *const ProgressContext<'a> = &raw const *context;
        let context_addr = context_ptr as usize;
        let handle = LeanCallbackHandle::<LeanProgressTick>::register(move |event| {
            // SAFETY: `context_addr` points at the `ProgressContext`
            // boxed inside the owning `ProgressBridge`. The bridge keeps
            // the callback handle live only for one synchronous Lean call
            // and drops the handle before the context. Host progress
            // shims do not store callback handles for later use.
            let context = unsafe { &*(context_addr as *const ProgressContext<'_>) };
            context.sink.report(LeanProgressEvent {
                phase: context.phase,
                current: event.current,
                total: context.total,
                elapsed: context.started.elapsed(),
            });
            LeanCallbackFlow::Continue
        })?;
        Ok(Self { handle, context })
    }

    pub(crate) fn abi_parts(&self) -> (usize, usize) {
        self.handle.abi_parts()
    }

    pub(crate) fn decode<'lean, T>(&self, obj: Obj<'lean>) -> LeanResult<T>
    where
        T: TryFromLean<'lean>,
    {
        decode_progress_result(obj, &self.handle)
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

fn decode_progress_result<'lean, T>(obj: Obj<'lean>, handle: &LeanCallbackHandle<LeanProgressTick>) -> LeanResult<T>
where
    T: TryFromLean<'lean>,
{
    match ctor_tag(&obj)? {
        1 => {
            let [value] = take_ctor_objects::<1>(obj, 1, "Except.ok")?;
            T::try_from_lean(value)
        }
        0 => {
            let [status_obj] = take_ctor_objects::<1>(obj, 0, "Except.error")?;
            let status = u8::try_from_lean(status_obj)?;
            progress_status_to_result(status, handle)?;
            Err(lean_rs::__host_internals::host_internal(
                "progress shim returned Except.error with successful callback status",
            ))
        }
        other => Err(conversion_error(format!(
            "expected Lean Except ctor from progress shim (tag 0 = error, 1 = ok), found tag {other}"
        ))),
    }
}

fn progress_status_to_result(status: u8, handle: &LeanCallbackHandle<LeanProgressTick>) -> LeanResult<()> {
    match LeanCallbackStatus::from_abi(status) {
        Some(LeanCallbackStatus::Ok) => Ok(()),
        Some(LeanCallbackStatus::StaleHandle) => Err(lean_rs::__host_internals::host_internal(
            "Lean progress shim called a stale callback handle",
        )),
        Some(LeanCallbackStatus::WrongPayload) => Err(lean_rs::__host_internals::host_internal(
            "Lean progress shim called a callback handle through the wrong payload trampoline",
        )),
        Some(LeanCallbackStatus::Stopped) => Err(lean_rs::__host_internals::host_internal(
            "progress sink asked Lean to stop, but host progress does not define stop semantics",
        )),
        Some(LeanCallbackStatus::Panic) => Err(handle.last_error().unwrap_or_else(|| {
            lean_rs::__host_internals::host_internal("progress sink panicked without recording a callback error")
        })),
        None => Err(conversion_error(format!(
            "Lean progress shim returned unknown callback status byte {status}"
        ))),
    }
}
