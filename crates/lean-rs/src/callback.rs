//! Rust callback handles for Lean-to-Rust interop.
//!
//! This module is an L1 interop primitive. It owns the Rust side of the
//! callback ABI: handle lifetime, trampoline selection, payload decoding,
//! stale-handle checks, and panic containment. Lean receives two `USize`
//! values, an opaque handle and the crate-owned trampoline, then calls back
//! into Rust with one of the sealed payload shapes supported by this crate.
//!
//! The public surface deliberately does not accept a user-supplied function
//! pointer. Callers register a Rust closure and pass the returned
//! [`LeanCallbackHandle`]'s ABI values to a Lean export. The handle must stay
//! alive until Lean can no longer call it.

// SAFETY DOC: string callbacks receive a borrowed Lean `String` object from
// the generic interop shim. The trampoline validates the Lean shape, copies
// bytes into an owned Rust `String`, and never decrements the borrowed object.
#![allow(unsafe_code)]

use std::collections::HashMap;
use std::marker::PhantomData;
use std::num::NonZeroUsize;
use std::slice;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use lean_rs_sys::lean_object;
use lean_rs_sys::object::{lean_is_scalar, lean_is_string};
use lean_rs_sys::string::{lean_string_cstr, lean_string_size};

use crate::error::panic::catch_callback_panic;
use crate::error::{LeanError, LeanResult};

type ProgressCallbackFn = dyn Fn(LeanProgressTick) -> LeanCallbackFlow + Send + Sync + 'static;
type StringCallbackFn = dyn Fn(LeanStringEvent) -> LeanCallbackFlow + Send + Sync + 'static;

const PAYLOAD_PROGRESS_TICK: u8 = 0;
const PAYLOAD_STRING: u8 = 1;

static NEXT_ID: AtomicUsize = AtomicUsize::new(1);
static REGISTRY: OnceLock<Mutex<HashMap<usize, Arc<CallbackEntry>>>> = OnceLock::new();

/// Payload type accepted by a [`LeanCallbackHandle`].
///
/// This trait is sealed. Downstream crates can use the payload types provided
/// by `lean-rs`, but cannot implement new callback ABI shapes. That keeps
/// Lean object lifetimes, payload decoding, wrong-payload checks, and
/// trampoline safety inside this crate.
#[allow(private_bounds, reason = "standard sealed-trait pattern keeps payload ABI private")]
pub trait LeanCallbackPayload: private::Sealed + Send + Sync + 'static {}

/// Counter payload for progress-like callback ticks.
///
/// `lean-rs-host` maps this payload into host progress events; `lean-rs`
/// itself attaches no theorem-prover policy to the counters.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LeanProgressTick {
    /// Current item, tick, or phase-local counter supplied by Lean.
    pub current: u64,
    /// Total item count or phase-local bound supplied by Lean.
    pub total: u64,
}

/// String payload delivered by Lean and copied before user code runs.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LeanStringEvent {
    /// Owned UTF-8 string copied from Lean before invoking the callback.
    pub value: String,
}

/// Flow decision returned by a Rust callback.
///
/// Lean shims should continue their callback loop only when the trampoline
/// returns [`LeanCallbackStatus::Ok`]. Returning [`Stop`](Self::Stop) asks the
/// Lean loop to stop cleanly and return [`LeanCallbackStatus::Stopped`).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LeanCallbackFlow {
    /// Continue the Lean-side callback loop.
    Continue,
    /// Stop the Lean-side callback loop without treating the callback as a
    /// panic or stale-handle failure.
    Stop,
}

/// Status returned by the Rust callback trampoline to Lean.
///
/// Lean shims should treat any value other than [`Ok`](Self::Ok) as a request
/// to stop the current callback loop and return the status to Rust.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum LeanCallbackStatus {
    /// The callback ran successfully.
    Ok = 0,
    /// Lean called an id that is no longer registered.
    StaleHandle = 1,
    /// The registered Rust callback panicked and the trampoline contained it.
    Panic = 2,
    /// Lean called a handle through a trampoline for the wrong payload type.
    WrongPayload = 3,
    /// The registered Rust callback asked Lean to stop cleanly.
    Stopped = 4,
}

impl LeanCallbackStatus {
    /// Decode a status byte returned by a Lean callback shim.
    #[must_use]
    pub const fn from_abi(value: u8) -> Option<Self> {
        match value {
            0 => Some(Self::Ok),
            1 => Some(Self::StaleHandle),
            2 => Some(Self::Panic),
            3 => Some(Self::WrongPayload),
            4 => Some(Self::Stopped),
            _ => None,
        }
    }

    /// Encode this status for the Lean `UInt8` ABI.
    #[must_use]
    pub const fn as_abi(self) -> u8 {
        self as u8
    }

    /// Stable diagnostic text for callback-shim status handling.
    #[must_use]
    pub const fn description(self) -> &'static str {
        match self {
            Self::Ok => "callback completed successfully",
            Self::StaleHandle => "Lean called a callback handle after Rust dropped it",
            Self::Panic => "Rust callback panicked and the trampoline contained the panic",
            Self::WrongPayload => "Lean called a callback handle through the wrong payload trampoline",
            Self::Stopped => "Rust callback asked Lean to stop the callback loop",
        }
    }
}

/// RAII registration for a Rust callback Lean may invoke.
///
/// Register with a supported payload specialization, pass
/// [`LeanCallbackHandle::abi_parts`] to a Lean export whose first two arguments
/// are `USize`, and keep the handle alive until the Lean side cannot call it
/// again. Dropping the handle unregisters its id; a later Lean call with the
/// same stale id returns [`LeanCallbackStatus::StaleHandle`] instead of
/// dereferencing freed Rust memory.
///
/// The callback runs synchronously on the Lean-bound thread that invoked the
/// Lean export. It must not call back into the same `LeanSession` or re-enter
/// the same Lean call stack. Rust panics are caught inside the trampoline and
/// recorded as [`LeanError`] with [`crate::HostStage::CallbackPanic`]; aborting
/// panics and Lean internal panics remain process-scoped.
///
/// `LeanCallbackHandle` is [`Send`] and [`Sync`] because registry lookup clones
/// an internal [`Arc`] before running the callback, and registration/removal is
/// guarded by a mutex. The registered closure must therefore be
/// `Send + Sync + 'static`.
pub struct LeanCallbackHandle<P: LeanCallbackPayload> {
    id: NonZeroUsize,
    entry: Arc<CallbackEntry>,
    _payload: PhantomData<fn(P)>,
}

impl<P: LeanCallbackPayload> std::fmt::Debug for LeanCallbackHandle<P> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LeanCallbackHandle")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

impl LeanCallbackHandle<LeanProgressTick> {
    /// Register a Rust callback for progress tick payloads.
    ///
    /// # Errors
    ///
    /// Returns [`LeanError::Host`] with diagnostic code
    /// [`crate::LeanDiagnosticCode::Internal`] if the registry cannot allocate
    /// a fresh nonzero id. This requires exhausting the process-size `usize`
    /// id space while many handles are still live.
    pub fn register<F>(callback: F) -> LeanResult<Self>
    where
        F: Fn(LeanProgressTick) -> LeanCallbackFlow + Send + Sync + 'static,
    {
        register_entry(CallbackEntry::new_progress(callback))
    }
}

impl LeanCallbackHandle<LeanStringEvent> {
    /// Register a Rust callback for string payloads.
    ///
    /// The Lean string is copied into an owned [`String`] before user code
    /// runs, so no Lean object lifetime escapes the trampoline.
    ///
    /// # Errors
    ///
    /// Returns [`LeanError::Host`] with diagnostic code
    /// [`crate::LeanDiagnosticCode::Internal`] if the registry cannot allocate
    /// a fresh nonzero id.
    pub fn register<F>(callback: F) -> LeanResult<Self>
    where
        F: Fn(LeanStringEvent) -> LeanCallbackFlow + Send + Sync + 'static,
    {
        register_entry(CallbackEntry::new_string(callback))
    }
}

impl<P: LeanCallbackPayload> LeanCallbackHandle<P> {
    /// Opaque `USize` handle to pass as the first Lean callback argument.
    #[must_use]
    pub fn abi_handle(&self) -> usize {
        self.id.get()
    }

    /// Crate-owned trampoline value to pass as the second Lean callback
    /// argument.
    ///
    /// Callers may pass this value to Lean, but they never construct or supply
    /// a trampoline function pointer themselves.
    #[must_use]
    pub fn abi_trampoline(&self) -> usize {
        P::trampoline()
    }

    /// Return `(handle, trampoline)` for Lean exports using the standard
    /// two-`USize` callback ABI.
    #[must_use]
    pub fn abi_parts(&self) -> (usize, usize) {
        (self.abi_handle(), self.abi_trampoline())
    }

    /// Last Rust error recorded by this callback handle.
    ///
    /// This is currently populated when the callback panics and the trampoline
    /// returns [`LeanCallbackStatus::Panic`]. Stale-handle calls happen after
    /// the handle was dropped, so no live handle exists to store that status.
    #[must_use]
    pub fn last_error(&self) -> Option<LeanError> {
        self.entry.last_error()
    }
}

impl<P: LeanCallbackPayload> Drop for LeanCallbackHandle<P> {
    fn drop(&mut self) {
        if let Some(registry) = REGISTRY.get()
            && let Ok(mut guard) = registry.lock()
        {
            drop(guard.remove(&self.id.get()));
        }
    }
}

enum CallbackEntryKind {
    Progress(Box<ProgressCallbackFn>),
    String(Box<StringCallbackFn>),
}

struct CallbackEntry {
    kind: CallbackEntryKind,
    last_error: Mutex<Option<LeanError>>,
}

impl std::fmt::Debug for CallbackEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CallbackEntry").finish_non_exhaustive()
    }
}

impl CallbackEntry {
    fn new_progress<F>(callback: F) -> Self
    where
        F: Fn(LeanProgressTick) -> LeanCallbackFlow + Send + Sync + 'static,
    {
        Self {
            kind: CallbackEntryKind::Progress(Box::new(callback)),
            last_error: Mutex::new(None),
        }
    }

    fn new_string<F>(callback: F) -> Self
    where
        F: Fn(LeanStringEvent) -> LeanCallbackFlow + Send + Sync + 'static,
    {
        Self {
            kind: CallbackEntryKind::String(Box::new(callback)),
            last_error: Mutex::new(None),
        }
    }

    fn report_progress(&self, event: LeanProgressTick) -> LeanCallbackStatus {
        let CallbackEntryKind::Progress(callback) = &self.kind else {
            return LeanCallbackStatus::WrongPayload;
        };
        let result = catch_callback_panic(|| Ok(callback(event)));
        self.flow_or_panic(result)
    }

    fn report_string(&self, event: LeanStringEvent) -> LeanCallbackStatus {
        let CallbackEntryKind::String(callback) = &self.kind else {
            return LeanCallbackStatus::WrongPayload;
        };
        let result = catch_callback_panic(|| Ok(callback(event)));
        self.flow_or_panic(result)
    }

    fn flow_or_panic(&self, result: LeanResult<LeanCallbackFlow>) -> LeanCallbackStatus {
        match result {
            Ok(LeanCallbackFlow::Continue) => LeanCallbackStatus::Ok,
            Ok(LeanCallbackFlow::Stop) => LeanCallbackStatus::Stopped,
            Err(err) => {
                if let Ok(mut last_error) = self.last_error.lock() {
                    *last_error = Some(err);
                }
                LeanCallbackStatus::Panic
            }
        }
    }

    fn last_error(&self) -> Option<LeanError> {
        self.last_error.lock().ok().and_then(|guard| guard.clone())
    }
}

fn register_entry<P: LeanCallbackPayload>(entry: CallbackEntry) -> LeanResult<LeanCallbackHandle<P>> {
    let entry = Arc::new(entry);
    let registry = registry();
    let mut guard = registry
        .lock()
        .map_err(|_| LeanError::internal("callback registry mutex was poisoned during registration"))?;
    let id = allocate_id(&guard)?;
    let previous = guard.insert(id.get(), Arc::clone(&entry));
    debug_assert!(previous.is_none(), "fresh callback id collided with an existing entry");
    drop(guard);
    Ok(LeanCallbackHandle {
        id,
        entry,
        _payload: PhantomData,
    })
}

fn registry() -> &'static Mutex<HashMap<usize, Arc<CallbackEntry>>> {
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

fn allocate_id(guard: &HashMap<usize, Arc<CallbackEntry>>) -> LeanResult<NonZeroUsize> {
    for _ in 0..1024 {
        let raw = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let Some(id) = NonZeroUsize::new(raw) else {
            continue;
        };
        if !guard.contains_key(&id.get()) {
            return Ok(id);
        }
    }
    Err(LeanError::internal(
        "callback registry could not allocate a fresh nonzero handle id",
    ))
}

extern "C" fn progress_trampoline(
    handle: usize,
    payload_tag: u8,
    arg0: u64,
    arg1: u64,
    _payload: *mut lean_object,
) -> u8 {
    if payload_tag != PAYLOAD_PROGRESS_TICK {
        return LeanCallbackStatus::WrongPayload.as_abi();
    }
    let entry = registry().lock().ok().and_then(|guard| guard.get(&handle).cloned());
    let Some(entry) = entry else {
        return LeanCallbackStatus::StaleHandle.as_abi();
    };
    entry
        .report_progress(LeanProgressTick {
            current: arg0,
            total: arg1,
        })
        .as_abi()
}

extern "C" fn string_trampoline(
    handle: usize,
    payload_tag: u8,
    _arg0: u64,
    _arg1: u64,
    payload: *mut lean_object,
) -> u8 {
    if payload_tag != PAYLOAD_STRING {
        return LeanCallbackStatus::WrongPayload.as_abi();
    }
    let entry = registry().lock().ok().and_then(|guard| guard.get(&handle).cloned());
    let Some(entry) = entry else {
        return LeanCallbackStatus::StaleHandle.as_abi();
    };
    let Some(value) = decode_string_payload(payload) else {
        return LeanCallbackStatus::WrongPayload.as_abi();
    };
    entry.report_string(LeanStringEvent { value }).as_abi()
}

fn decode_string_payload(payload: *mut lean_object) -> Option<String> {
    if payload.is_null() {
        return None;
    }
    // SAFETY: scalar check inspects pointer bits only and is valid for every
    // Lean-shaped value the trampoline may receive.
    if unsafe { lean_is_scalar(payload) } {
        return None;
    }
    // SAFETY: the generic string callback shim passes `payload : @& String`.
    // Wrong-payload tests route through null/scalar-shaped payloads or a
    // mismatched handle and return before this heap predicate.
    if !unsafe { lean_is_string(payload) } {
        return None;
    }
    // SAFETY: kind verified; the string is borrowed for the duration of the
    // extern call. Copy the bytes into Rust before invoking user code so no
    // Lean object lifetime escapes the trampoline.
    let bytes = unsafe {
        let size_with_nul = lean_string_size(payload);
        let len = size_with_nul.saturating_sub(1);
        let data = lean_string_cstr(payload).cast::<u8>();
        slice::from_raw_parts(data, len)
    };
    String::from_utf8(bytes.to_vec()).ok()
}

mod private {
    use super::{LeanProgressTick, LeanStringEvent, progress_trampoline, string_trampoline};

    pub trait Sealed {
        fn trampoline() -> usize;
    }

    impl Sealed for LeanProgressTick {
        fn trampoline() -> usize {
            progress_trampoline as *const () as usize
        }
    }

    impl Sealed for LeanStringEvent {
        fn trampoline() -> usize {
            string_trampoline as *const () as usize
        }
    }
}

impl LeanCallbackPayload for LeanProgressTick {}
impl LeanCallbackPayload for LeanStringEvent {}

#[cfg(test)]
mod tests {
    use super::{LeanCallbackFlow, LeanCallbackHandle, LeanCallbackStatus, LeanProgressTick, LeanStringEvent};

    #[test]
    fn callback_handle_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<LeanCallbackHandle<LeanProgressTick>>();
        assert_send_sync::<LeanCallbackHandle<LeanStringEvent>>();
    }

    #[test]
    fn status_bytes_round_trip() {
        assert_eq!(LeanCallbackStatus::from_abi(0), Some(LeanCallbackStatus::Ok));
        assert_eq!(LeanCallbackStatus::from_abi(1), Some(LeanCallbackStatus::StaleHandle),);
        assert_eq!(LeanCallbackStatus::from_abi(2), Some(LeanCallbackStatus::Panic));
        assert_eq!(LeanCallbackStatus::from_abi(3), Some(LeanCallbackStatus::WrongPayload));
        assert_eq!(LeanCallbackStatus::from_abi(4), Some(LeanCallbackStatus::Stopped));
        assert_eq!(LeanCallbackStatus::from_abi(5), None);
    }

    #[test]
    fn flow_is_explicit() {
        assert_ne!(LeanCallbackFlow::Continue, LeanCallbackFlow::Stop);
    }
}
