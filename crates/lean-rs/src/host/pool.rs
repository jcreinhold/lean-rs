//! Session pooling for amortising `Lean.importModules` across reused
//! environments.
//!
//! Re-importing the Lean prelude is the dominant FFI cost on the host
//! stack — measured on a dev macOS rig at roughly 4×–5× the cost of
//! reusing an existing session (see the `session_reuse_amortises_import`
//! timing note in `host/tests.rs`). [`SessionPool`] keeps a bounded
//! free-list of previously imported `Lean.Environment` values keyed by
//! their imports list; on [`SessionPool::acquire`], a matching entry is
//! popped and rewrapped under the caller-supplied
//! [`crate::host::LeanCapabilities`] borrow, and on
//! [`PooledSession::drop`], the environment goes back to the pool (or is
//! released if capacity is full).
//!
//! ## Capability-agnostic storage
//!
//! Entries store the bare imported environment as
//! `Obj<'lean>` (a refcounted handle to the Lean
//! `Environment` value), not a full [`crate::LeanSession`]. The session
//! borrows from the capability via `'c`; storing one in the pool would
//! tie the pool's lifetime to a single capability borrow. Storing the
//! bare environment instead lets each [`SessionPool::acquire`] thread a
//! fresh capability borrow without touching `'lean`. Environments are
//! Lean values bound to the runtime, not to the capability that imported
//! them, so this rewrapping is semantically free.
//!
//! ## Capacity policy
//!
//! [`SessionPool::with_capacity`] sets a hard upper bound on the
//! free-list size. On release, if the pool is at capacity, the
//! environment is dropped immediately (its `Obj<'lean>`
//! `Drop` runs `lean_dec` and the underlying allocation is freed). The
//! free list is FIFO on `take` and LRU on `push`, so the most
//! recently-released environment is the next to be reused — hot OS
//! caches stay warm. There is no eviction-by-age or eviction-by-distinct-key
//! policy beyond the capacity bound.
//!
//! ## Threading
//!
//! [`SessionPool`] is `!Send + !Sync` (inherited from the contained
//! `Obj<'lean>` and the `RefCell` that wraps the free list). The pool
//! is a per-thread reuse helper; cross-thread pooling is explicitly out
//! of scope (per prompt 20). Per-pool stats are `Cell<PoolStats>` —
//! single-threaded but uniform with the per-session
//! [`crate::host::session::SessionStats`] story.

use core::cell::{Cell, RefCell};

use crate::error::LeanResult;
use crate::host::capabilities::LeanCapabilities;
use crate::host::session::LeanSession;
use crate::runtime::LeanRuntime;
use crate::runtime::obj::Obj;

// -- PoolStats: pool-level reuse metrics ---------------------------------

/// Cumulative metrics for one [`SessionPool`].
///
/// Snapshot via [`SessionPool::stats`]. Counters never reset — to
/// compute a delta, take two snapshots and subtract.
///
/// `imports_performed + reused == acquired` by construction: every
/// [`SessionPool::acquire`] call increments `acquired` exactly once
/// plus either `imports_performed` (cache miss) or `reused` (cache
/// hit). Similarly, `released_to_pool + released_dropped` counts every
/// [`PooledSession::drop`] firing — they only diverge in proportion
/// once the pool exceeds capacity.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct PoolStats {
    /// Number of fresh `Lean.importModules` calls performed because no
    /// pooled environment matched the requested imports list.
    pub imports_performed: u64,
    /// Number of acquire calls that found a matching pooled environment
    /// and reused it instead of re-importing.
    pub reused: u64,
    /// Total acquire calls (== `imports_performed + reused`).
    pub acquired: u64,
    /// Number of release events that pushed the environment back onto
    /// the free list.
    pub released_to_pool: u64,
    /// Number of release events that dropped the environment because
    /// the pool was at capacity.
    pub released_dropped: u64,
}

// -- ImportsKey: hashable cache key for the imports list -----------------

/// Free-list key: the ordered imports list a pooled environment was
/// imported with.
///
/// Order matters because [`Lean.importModules`] is order-sensitive — a
/// later import can shadow an earlier one. Equality is structural and
/// canonical (the same `&[&str]` always produces the same key).
#[derive(Clone, Eq, PartialEq)]
struct ImportsKey(Vec<String>);

impl ImportsKey {
    fn from_slice(imports: &[&str]) -> Self {
        Self(imports.iter().map(|&s| s.to_owned()).collect())
    }
}

// -- PooledEntry: one slot on the free list ------------------------------

struct PooledEntry<'lean> {
    imports_key: ImportsKey,
    environment: Obj<'lean>,
}

// -- PoolInner: RefCell-protected free list ------------------------------

struct PoolInner<'lean> {
    /// FIFO on take, LIFO on push (newest entries near the back; the
    /// most-recently-released entry matching a given imports key is the
    /// one acquire pops). The list scan is linear, which is fine for
    /// the small capacities this pool is sized for — pooling is for
    /// amortising imports across O(10s) of sessions, not for managing
    /// thousands.
    free: Vec<PooledEntry<'lean>>,
}

impl<'lean> PoolInner<'lean> {
    /// Pop the most recently released entry whose imports key matches.
    fn take_matching(&mut self, key: &ImportsKey) -> Option<Obj<'lean>> {
        let idx = self.free.iter().rposition(|entry| &entry.imports_key == key)?;
        Some(self.free.remove(idx).environment)
    }
}

// -- SessionPool ---------------------------------------------------------

/// A capacity-bounded reuse pool of imported Lean environments.
///
/// Built with [`Self::with_capacity`]; environments enter the pool
/// through [`PooledSession::drop`] (returning a previously-acquired
/// session). Pool entries are capability-agnostic: a single pool may be
/// shared across multiple [`LeanCapabilities`] values, as long as they
/// share the same runtime.
///
/// Neither [`Send`] nor [`Sync`] (inherited from the contained
/// `Obj<'lean>` values).
pub struct SessionPool<'lean> {
    runtime: &'lean LeanRuntime,
    capacity: usize,
    inner: RefCell<PoolInner<'lean>>,
    stats: Cell<PoolStats>,
}

impl<'lean> SessionPool<'lean> {
    /// Build an empty pool with hard upper bound `capacity` on stored
    /// environments.
    ///
    /// A `capacity` of 0 disables reuse — every [`Self::acquire`] call
    /// imports fresh and every release drops the environment. This is
    /// useful for tests that want metrics without recycling, and as the
    /// degenerate point that proves the pool's metrics agree with
    /// repeated `caps.session(...)` calls.
    ///
    /// The `runtime` borrow witnesses `'lean` and is stored so the pool
    /// itself outlives every entry on its free list — even after every
    /// [`PooledSession`] has been dropped, the pool retains a usable
    /// runtime reference.
    #[must_use]
    pub fn with_capacity(runtime: &'lean LeanRuntime, capacity: usize) -> Self {
        Self {
            runtime,
            capacity,
            inner: RefCell::new(PoolInner {
                free: Vec::with_capacity(capacity),
            }),
            stats: Cell::new(PoolStats::default()),
        }
    }

    /// Acquire a session targeting `imports` under `caps`.
    ///
    /// If a pooled environment was previously released with the same
    /// `imports` list (order-sensitive), it is rewrapped under the
    /// supplied capability borrow and returned — no `Lean.importModules`
    /// runs. Otherwise the pool calls
    /// [`LeanCapabilities::session`] internally to perform a fresh
    /// import. Either way, the resulting [`PooledSession`] returns the
    /// underlying environment to the pool on `Drop`.
    ///
    /// `caps` must come from the same [`LeanRuntime`] the pool was
    /// constructed with; this is structurally enforced by the shared
    /// `'lean` lifetime parameter.
    ///
    /// # Errors
    ///
    /// Returns [`crate::LeanError::LeanException`] if a fresh import is
    /// required and the Lean-side `lean_rs_host_session_import` shim
    /// raises through `IO`. Cached environments never re-fail.
    pub fn acquire<'p, 'c>(
        &'p self,
        caps: &'c LeanCapabilities<'lean, 'c>,
        imports: &[&str],
    ) -> LeanResult<PooledSession<'lean, 'p, 'c>> {
        let _span = tracing::debug_span!(
            target: "lean_rs",
            "lean_rs.host.pool.acquire",
            imports_len = imports.len(),
            imports_first = imports.first().copied().unwrap_or("<empty>"),
        )
        .entered();
        debug_assert!(
            core::ptr::eq(self.runtime, caps.host().runtime()),
            "pool runtime and capability runtime must agree; the shared 'lean parameter normally enforces this",
        );
        let key = ImportsKey::from_slice(imports);
        let (session, hit) = {
            let mut inner = self.inner.borrow_mut();
            if let Some(env) = inner.take_matching(&key) {
                self.bump_reused();
                (LeanSession::from_environment(caps, env), true)
            } else {
                drop(inner);
                let session = caps.session(imports)?;
                self.bump_imported();
                (session, false)
            }
        };
        tracing::debug!(target: "lean_rs", hit = hit, "lean_rs.host.pool.acquire.result");
        Ok(PooledSession {
            pool: self,
            imports_key: key,
            session: Some(session),
        })
    }

    /// Snapshot of accumulated pool metrics.
    #[must_use]
    pub fn stats(&self) -> PoolStats {
        self.stats.get()
    }

    /// Current number of environments on the free list.
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner.borrow().free.len()
    }

    /// `true` iff the free list is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Configured capacity (the upper bound passed to
    /// [`Self::with_capacity`]).
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    fn bump_reused(&self) {
        let mut s = self.stats.get();
        s.reused = s.reused.saturating_add(1);
        s.acquired = s.acquired.saturating_add(1);
        self.stats.set(s);
    }

    fn bump_imported(&self) {
        let mut s = self.stats.get();
        s.imports_performed = s.imports_performed.saturating_add(1);
        s.acquired = s.acquired.saturating_add(1);
        self.stats.set(s);
    }

    fn release(&self, key: ImportsKey, env: Obj<'lean>) {
        let mut inner = self.inner.borrow_mut();
        let mut s = self.stats.get();
        let kept = inner.free.len() < self.capacity;
        if kept {
            inner.free.push(PooledEntry {
                imports_key: key,
                environment: env,
            });
            s.released_to_pool = s.released_to_pool.saturating_add(1);
        } else {
            // Drop `env`: its `Obj` Drop runs `lean_dec` and the
            // environment allocation is freed if the refcount reaches 0.
            drop(env);
            s.released_dropped = s.released_dropped.saturating_add(1);
        }
        self.stats.set(s);
        tracing::trace!(
            target: "lean_rs",
            kept = kept,
            "lean_rs.host.pool.release",
        );
    }
}

impl core::fmt::Debug for SessionPool<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("SessionPool")
            .field("capacity", &self.capacity)
            .field("len", &self.len())
            .field("stats", &self.stats.get())
            .finish()
    }
}

// -- PooledSession -------------------------------------------------------

/// A [`LeanSession`] borrowed from a [`SessionPool`].
///
/// Behaves as a [`LeanSession`] through [`core::ops::Deref`] /
/// [`core::ops::DerefMut`] — every session method is reachable directly:
///
/// ```ignore
/// let pool = lean_rs::SessionPool::with_capacity(runtime, 4);
/// let mut sess = pool.acquire(&caps, &["MyLib"])?;
/// let kind = sess.declaration_kind("MyLib.thing")?;
/// // dropping `sess` returns the imported environment to the pool
/// ```
///
/// On `Drop`, the underlying imported environment is returned to the
/// pool (or released if the pool is at capacity). Per-session
/// [`crate::host::session::SessionStats`] are scoped to the lifetime of
/// this checkout — they start at zero on every acquire and are
/// inaccessible after release.
///
/// Three lifetimes: `'lean` (runtime), `'p` (pool borrow), `'c`
/// (capability borrow). Neither [`Send`] nor [`Sync`] (inherited from
/// the contained [`LeanSession`]).
pub struct PooledSession<'lean, 'p, 'c> {
    pool: &'p SessionPool<'lean>,
    imports_key: ImportsKey,
    /// `Option` so [`Drop`] can take the session by value without
    /// resorting to `ManuallyDrop`. Always `Some` between
    /// construction and `Drop`.
    session: Option<LeanSession<'lean, 'c>>,
}

impl<'lean, 'c> core::ops::Deref for PooledSession<'lean, '_, 'c> {
    type Target = LeanSession<'lean, 'c>;

    // PROOF OBLIGATION: `session` is initialised to `Some` at the only
    // construction site (`SessionPool::acquire`) and is taken to `None`
    // exactly once, inside `Drop::drop`. `Deref::deref` is only callable
    // through a `&self` borrow, which is not possible during `Drop`, so
    // observing `None` here is structurally impossible.
    #[allow(clippy::expect_used, reason = "see PROOF OBLIGATION above")]
    fn deref(&self) -> &Self::Target {
        self.session
            .as_ref()
            .expect("session is Some between PooledSession::acquire and Drop::drop")
    }
}

#[allow(
    single_use_lifetimes,
    clippy::elidable_lifetime_names,
    reason = "the named lifetimes line up with `Deref::Target = LeanSession<'lean, 'c>` above; \
              elision flips the inferred bound and breaks the trait-signature check"
)]
impl<'lean, 'c> core::ops::DerefMut for PooledSession<'lean, '_, 'c> {
    // Same PROOF OBLIGATION as the `Deref` impl above: `DerefMut::deref_mut`
    // is unreachable from inside `Drop::drop`, so `session` is always
    // `Some` here.
    #[allow(clippy::expect_used, reason = "see PROOF OBLIGATION on Deref impl")]
    fn deref_mut(&mut self) -> &mut LeanSession<'lean, 'c> {
        self.session
            .as_mut()
            .expect("session is Some between PooledSession::acquire and Drop::drop")
    }
}

impl Drop for PooledSession<'_, '_, '_> {
    fn drop(&mut self) {
        if let Some(session) = self.session.take() {
            let env = session.into_environment();
            self.pool.release(self.imports_key.clone(), env);
        }
    }
}

impl core::fmt::Debug for PooledSession<'_, '_, '_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("PooledSession").finish_non_exhaustive()
    }
}
