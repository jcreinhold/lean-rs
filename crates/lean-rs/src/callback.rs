//! Rust callback handles for Lean-to-Rust interop.
//!
//! This module is an L1 interop primitive. It owns the Rust side of the
//! callback ABI proven in `docs/architecture/09-callback-abi-spike.md`:
//! Lean receives two `USize` values, an opaque handle and the crate-owned
//! trampoline, then calls back into Rust with a fixed `(current, total)`
//! integer payload.
//!
//! The public surface deliberately does not accept a user-supplied function
//! pointer. Callers register a Rust closure and pass the returned
//! [`LeanCallbackHandle`]'s ABI values to a Lean export. The handle must stay
//! alive until Lean can no longer call it.

use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use crate::error::panic::catch_callback_panic;
use crate::error::{LeanError, LeanResult};

type CallbackFn = dyn Fn(LeanCallbackEvent) + Send + Sync + 'static;

static NEXT_ID: AtomicUsize = AtomicUsize::new(1);
static REGISTRY: OnceLock<Mutex<HashMap<usize, Arc<CallbackEntry>>>> = OnceLock::new();

/// Payload delivered by the L1 callback trampoline.
///
/// The shape is intentionally small: two `u64` counters. Higher-level crates
/// may interpret them as progress, batch index, or another domain value, but
/// this crate does not attach host-session policy to them.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LeanCallbackEvent {
    /// Current item, tick, or phase-local counter supplied by Lean.
    pub current: u64,
    /// Total item count or phase-local bound supplied by Lean.
    pub total: u64,
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
}

impl LeanCallbackStatus {
    /// Decode a status byte returned by a Lean callback shim.
    #[must_use]
    pub const fn from_abi(value: u8) -> Option<Self> {
        match value {
            0 => Some(Self::Ok),
            1 => Some(Self::StaleHandle),
            2 => Some(Self::Panic),
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
        }
    }
}

/// RAII registration for a Rust callback Lean may invoke.
///
/// Register with [`LeanCallbackHandle::register`], pass
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
#[derive(Debug)]
pub struct LeanCallbackHandle {
    id: NonZeroUsize,
    entry: Arc<CallbackEntry>,
}

impl LeanCallbackHandle {
    /// Register a Rust callback for Lean to invoke.
    ///
    /// # Errors
    ///
    /// Returns [`LeanError::Host`] with diagnostic code
    /// [`crate::LeanDiagnosticCode::Internal`] if the registry cannot allocate
    /// a fresh nonzero id. This requires exhausting the process-size `usize`
    /// id space while many handles are still live.
    pub fn register<F>(callback: F) -> LeanResult<Self>
    where
        F: Fn(LeanCallbackEvent) + Send + Sync + 'static,
    {
        let entry = Arc::new(CallbackEntry::new(callback));
        let registry = registry();
        let mut guard = registry
            .lock()
            .map_err(|_| LeanError::internal("callback registry mutex was poisoned during registration"))?;
        let id = allocate_id(&guard)?;
        let previous = guard.insert(id.get(), Arc::clone(&entry));
        debug_assert!(previous.is_none(), "fresh callback id collided with an existing entry");
        drop(guard);
        Ok(Self { id, entry })
    }

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
        callback_trampoline as *const () as usize
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

impl Drop for LeanCallbackHandle {
    fn drop(&mut self) {
        if let Some(registry) = REGISTRY.get()
            && let Ok(mut guard) = registry.lock()
        {
            drop(guard.remove(&self.id.get()));
        }
    }
}

struct CallbackEntry {
    callback: Box<CallbackFn>,
    last_error: Mutex<Option<LeanError>>,
}

impl std::fmt::Debug for CallbackEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CallbackEntry").finish_non_exhaustive()
    }
}

impl CallbackEntry {
    fn new<F>(callback: F) -> Self
    where
        F: Fn(LeanCallbackEvent) + Send + Sync + 'static,
    {
        Self {
            callback: Box::new(callback),
            last_error: Mutex::new(None),
        }
    }

    fn report(&self, event: LeanCallbackEvent) -> LeanCallbackStatus {
        let result = catch_callback_panic(|| {
            (self.callback)(event);
            Ok(())
        });
        match result {
            Ok(()) => LeanCallbackStatus::Ok,
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

extern "C" fn callback_trampoline(handle: usize, current: u64, total: u64) -> u8 {
    let entry = registry().lock().ok().and_then(|guard| guard.get(&handle).cloned());
    let Some(entry) = entry else {
        return LeanCallbackStatus::StaleHandle.as_abi();
    };
    entry.report(LeanCallbackEvent { current, total }).as_abi()
}

#[cfg(test)]
mod tests {
    use super::{LeanCallbackHandle, LeanCallbackStatus};

    #[test]
    fn callback_handle_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<LeanCallbackHandle>();
    }

    #[test]
    fn status_bytes_round_trip() {
        assert_eq!(LeanCallbackStatus::from_abi(0), Some(LeanCallbackStatus::Ok));
        assert_eq!(LeanCallbackStatus::from_abi(1), Some(LeanCallbackStatus::StaleHandle),);
        assert_eq!(LeanCallbackStatus::from_abi(2), Some(LeanCallbackStatus::Panic));
        assert_eq!(LeanCallbackStatus::from_abi(3), None);
    }
}
