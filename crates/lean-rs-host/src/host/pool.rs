//! Session pooling for amortising `Lean.importModules` across reused
//! environments.
//!
//! Re-importing the Lean prelude is the dominant FFI cost on the host
//! stack—measured on a dev macOS rig at roughly 4×–5× the cost of
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
//! recently-released environment is the next to be reused—hot OS
//! caches stay warm. There is no eviction-by-age or eviction-by-distinct-key
//! policy beyond the capacity bound.
//!
//! [`SessionPool::drain`] explicitly drops every cached free-list entry
//! without discarding the pool itself. It releases the Rust-owned
//! environment references the pool is holding; it does not reset Lean's
//! process-global runtime state, module initializer flags, interned
//! names, compacted `.olean` regions, or allocator arenas.
//!
//! ## Threading
//!
//! [`SessionPool`] is `!Send + !Sync` (inherited from the contained
//! `Obj<'lean>` and the `RefCell` that wraps the free list). The pool
//! is a per-thread reuse helper; cross-thread pooling is explicitly
//! out of scope. Per-pool stats are `Cell<PoolStats>`—
//! single-threaded but uniform with the per-session
//! [`crate::host::session::SessionStats`] story.

use core::cell::{Cell, RefCell};

use std::path::PathBuf;
#[cfg(not(target_os = "linux"))]
use std::process::Command;

use lean_rs::LeanRuntime;
use lean_rs::Obj;
use lean_rs::ResourceExhaustedFacts;
use lean_rs::error::LeanError;
use lean_rs::error::LeanResult;

use crate::host::cancellation::{LeanCancellationToken, check_cancellation};
use crate::host::capabilities::LeanCapabilities;
use crate::host::progress::LeanProgressSink;
use crate::host::session::{LeanImportStats, LeanSession, LeanSessionImportProfile};

// -- PoolStats: pool-level reuse metrics ---------------------------------

/// Cumulative metrics for one [`SessionPool`].
///
/// Snapshot via [`SessionPool::stats`]. Counters never reset—to
/// compute a delta, take two snapshots and subtract.
///
/// `imports_performed + reused == acquired` by construction: every
/// [`SessionPool::acquire`] call increments `acquired` exactly once
/// plus either `imports_performed` (cache miss) or `reused` (cache
/// hit). Similarly, `released_to_pool + released_dropped` counts every
/// [`PooledSession::drop`] firing. `released_to_pool` is cumulative:
/// an entry counted there may later be removed by [`SessionPool::drain`],
/// which records the removal in `drained`.
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
    /// Number of explicit [`SessionPool::drain`] calls.
    pub drains: u64,
    /// Number of cached environments dropped by explicit drains.
    pub drained: u64,
    /// Number of fresh imports refused by [`SessionPoolMemoryPolicy`].
    pub fresh_import_refusals: u64,
    /// Number of process RSS samples taken before fresh imports.
    pub rss_samples: u64,
    /// Number of process RSS samples that were unavailable.
    pub rss_samples_unavailable: u64,
    /// Number of acquire calls that matched a reusable session key.
    pub key_hits: u64,
    /// Number of acquire calls that could not reuse a session key.
    pub key_misses: u64,
    /// Number of distinct session keys observed by this pool.
    pub distinct_keys_seen: u64,
    /// Number of fresh imports avoided by key hits.
    pub fresh_imports_avoided: u64,
    /// Key misses because the pool had no reusable entry.
    pub miss_empty_pool: u64,
    /// Key misses because the pool has zero reuse capacity.
    pub miss_reuse_disabled: u64,
    /// Key misses because cached entries existed but none matched the requested key.
    pub miss_no_matching_key: u64,
    /// Most recent key-miss reason.
    pub last_miss_reason: Option<SessionPoolKeyMissReason>,
}

/// Why a same-process pool acquire could not reuse a warm session key.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionPoolKeyMissReason {
    EmptyPool,
    ReuseDisabled,
    NoMatchingKey,
}

impl SessionPoolKeyMissReason {
    pub const fn label(self) -> &'static str {
        match self {
            Self::EmptyPool => "empty_pool",
            Self::ReuseDisabled => "reuse_disabled",
            Self::NoMatchingKey => "no_matching_key",
        }
    }
}

/// Policy for refusing fresh imports in a same-process [`SessionPool`].
///
/// Reusing an already-imported environment does not grow Lean's process-global
/// import state. Fresh imports can, so this policy is checked only on cache
/// miss, immediately before `Lean.importModules` would run.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SessionPoolMemoryPolicy {
    max_fresh_imports: Option<u64>,
    max_rss_kib: Option<u64>,
}

impl SessionPoolMemoryPolicy {
    /// Disable import/RSS refusals.
    ///
    /// This preserves the historical [`SessionPool::with_capacity`] behavior
    /// and is appropriate only for short-lived processes, tests with tiny
    /// import counts, or explicit profiling workloads.
    #[must_use]
    pub fn disabled() -> Self {
        Self::default()
    }

    /// Refuse the next cache-miss import once `limit` fresh imports have
    /// already run through this pool.
    #[must_use]
    pub fn max_fresh_imports(mut self, limit: u64) -> Self {
        self.max_fresh_imports = Some(limit.max(1));
        self
    }

    /// Refuse the next cache-miss import when current process RSS is at or
    /// above `limit_kib`.
    #[must_use]
    pub fn max_rss_kib(mut self, limit_kib: u64) -> Self {
        self.max_rss_kib = Some(limit_kib.max(1));
        self
    }

    /// Return the configured fresh-import limit.
    #[must_use]
    pub fn max_fresh_imports_limit(&self) -> Option<u64> {
        self.max_fresh_imports
    }

    /// Return the configured process RSS ceiling in KiB.
    #[must_use]
    pub fn max_rss_kib_limit(&self) -> Option<u64> {
        self.max_rss_kib
    }
}

/// Configuration for a same-process [`SessionPool`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionPoolConfig {
    capacity: usize,
    memory_policy: SessionPoolMemoryPolicy,
}

impl SessionPoolConfig {
    /// Create a pool configuration with a fixed free-list capacity.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity,
            memory_policy: SessionPoolMemoryPolicy::disabled(),
        }
    }

    /// Set the policy used before cache-miss imports.
    #[must_use]
    pub fn memory_policy(mut self, policy: SessionPoolMemoryPolicy) -> Self {
        self.memory_policy = policy;
        self
    }

    /// Return the configured free-list capacity.
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Return the configured memory policy.
    #[must_use]
    pub fn memory_policy_ref(&self) -> &SessionPoolMemoryPolicy {
        &self.memory_policy
    }
}

// -- SessionPoolKey: cache key for imported environments -----------------

/// Free-list key: the imported-environment identity a pooled session was
/// imported with.
///
/// Order matters because `Lean.importModules` is order-sensitive—a
/// later import can shadow an earlier one. Equality is structural and
/// canonical (the same capability root, profile, and `&[&str]` always
/// produces the same key).
#[derive(Clone, Eq, PartialEq)]
struct SessionPoolKey {
    project_root: PathBuf,
    imports: Vec<String>,
    import_profile: LeanSessionImportProfile,
}

impl SessionPoolKey {
    fn from_capabilities(
        caps: &LeanCapabilities<'_, '_>,
        imports: &[&str],
        import_profile: LeanSessionImportProfile,
    ) -> Self {
        Self {
            project_root: caps.host().project().root().to_path_buf(),
            imports: imports.iter().map(|&s| s.to_owned()).collect(),
            import_profile,
        }
    }
}

// -- PooledEntry: one slot on the free list ------------------------------

struct PooledEntry<'lean> {
    key: SessionPoolKey,
    environment: Obj<'lean>,
    import_stats: LeanImportStats,
}

// -- PoolInner: RefCell-protected free list ------------------------------

struct PoolInner<'lean> {
    /// FIFO on take, LIFO on push (newest entries near the back; the
    /// most-recently-released entry matching a given imports key is the
    /// one acquire pops). The list scan is linear, which is fine for
    /// the small capacities this pool is sized for—pooling is for
    /// amortising imports across O(10s) of sessions, not for managing
    /// thousands.
    free: Vec<PooledEntry<'lean>>,
    seen_keys: Vec<SessionPoolKey>,
}

impl<'lean> PoolInner<'lean> {
    /// Pop the most recently released entry whose session key matches.
    fn take_matching(&mut self, key: &SessionPoolKey) -> Option<PooledEntry<'lean>> {
        let idx = self.free.iter().rposition(|entry| &entry.key == key)?;
        Some(self.free.remove(idx))
    }
}

// -- SessionPool ---------------------------------------------------------

/// A capacity-bounded reuse pool of imported Lean environments.
///
/// Built with [`Self::with_capacity`]; environments enter the pool
/// through [`PooledSession::drop`] (returning a previously-acquired
/// session). Pool entries are keyed by canonical Lake project root,
/// ordered imports, and import profile. A single pool may be shared
/// across multiple [`LeanCapabilities`] values with the same runtime;
/// roots and profiles still partition the reusable environments.
///
/// Neither [`Send`] nor [`Sync`] (inherited from the contained
/// `Obj<'lean>` values).
pub struct SessionPool<'lean> {
    runtime: &'lean LeanRuntime,
    capacity: usize,
    memory_policy: SessionPoolMemoryPolicy,
    inner: RefCell<PoolInner<'lean>>,
    last_import_stats: RefCell<Option<LeanImportStats>>,
    stats: Cell<PoolStats>,
}

impl<'lean> SessionPool<'lean> {
    /// Build an empty pool with hard upper bound `capacity` on stored
    /// environments.
    ///
    /// A `capacity` of 0 disables reuse—every [`Self::acquire`] call
    /// imports fresh and every release drops the environment. This is
    /// useful for tests that want metrics without recycling, and as the
    /// degenerate point that proves the pool's metrics agree with
    /// repeated `caps.session(..., None, None)` calls.
    ///
    /// The `runtime` borrow witnesses `'lean` and is stored so the pool
    /// itself outlives every entry on its free list—even after every
    /// [`PooledSession`] has been dropped, the pool retains a usable
    /// runtime reference.
    #[must_use]
    pub fn with_capacity(runtime: &'lean LeanRuntime, capacity: usize) -> Self {
        Self::with_config(runtime, SessionPoolConfig::new(capacity))
    }

    /// Build an empty pool from an explicit configuration.
    #[must_use]
    pub fn with_config(runtime: &'lean LeanRuntime, config: SessionPoolConfig) -> Self {
        let capacity = config.capacity;
        Self {
            runtime,
            capacity,
            memory_policy: config.memory_policy,
            inner: RefCell::new(PoolInner {
                free: Vec::with_capacity(capacity),
                seen_keys: Vec::new(),
            }),
            last_import_stats: RefCell::new(None),
            stats: Cell::new(PoolStats::default()),
        }
    }

    /// Build an empty pool with a fresh-import memory policy.
    #[must_use]
    pub fn with_memory_policy(runtime: &'lean LeanRuntime, capacity: usize, policy: SessionPoolMemoryPolicy) -> Self {
        Self::with_config(runtime, SessionPoolConfig::new(capacity).memory_policy(policy))
    }

    /// Acquire a session targeting `imports` under `caps`.
    ///
    /// If a pooled environment was previously released with the same
    /// canonical project root, default import profile, and ordered
    /// `imports` list, it is rewrapped under the supplied capability
    /// borrow and returned—no `Lean.importModules` runs. Otherwise the
    /// pool calls [`LeanCapabilities::session`] internally to perform a
    /// fresh import. Either way, the resulting [`PooledSession`] returns
    /// the underlying environment to the pool on `Drop`.
    ///
    /// `caps` must come from the same [`LeanRuntime`] the pool was
    /// constructed with; this is structurally enforced by the shared
    /// `'lean` lifetime parameter.
    ///
    /// # Errors
    ///
    /// Returns [`lean_rs::LeanError::Cancelled`] if `cancellation` is
    /// already cancelled before the pool can reuse or import an
    /// environment.
    ///
    /// Returns [`lean_rs::LeanError::LeanException`] if a fresh import is
    /// required and the Lean-side `lean_rs_host_session_import` shim
    /// raises through `IO`. Cached environments never re-fail.
    pub fn acquire<'p, 'c>(
        &'p self,
        caps: &'c LeanCapabilities<'lean, 'c>,
        imports: &[&str],
        cancellation: Option<&LeanCancellationToken>,
        progress: Option<&dyn LeanProgressSink>,
    ) -> LeanResult<PooledSession<'lean, 'p, 'c>> {
        self.acquire_with_profile(
            caps,
            imports,
            LeanSessionImportProfile::default(),
            cancellation,
            progress,
        )
    }

    /// Acquire a session targeting `imports` with an explicit import profile.
    ///
    /// This is the profile-aware variant of [`Self::acquire`]. Profiles are
    /// part of the session-safety key; a legacy compatibility import never
    /// aliases a lighter default-profile environment.
    ///
    /// # Errors
    ///
    /// Same as [`Self::acquire`].
    pub fn acquire_with_profile<'p, 'c>(
        &'p self,
        caps: &'c LeanCapabilities<'lean, 'c>,
        imports: &[&str],
        import_profile: LeanSessionImportProfile,
        cancellation: Option<&LeanCancellationToken>,
        progress: Option<&dyn LeanProgressSink>,
    ) -> LeanResult<PooledSession<'lean, 'p, 'c>> {
        let _span = tracing::debug_span!(
            target: "lean_rs",
            "lean_rs.host.pool.acquire",
            profile = import_profile.label(),
            imports_len = imports.len(),
            imports_first = imports.first().copied().unwrap_or("<empty>"),
        )
        .entered();
        check_cancellation(cancellation)?;
        debug_assert!(
            core::ptr::eq(self.runtime, caps.host().runtime()),
            "pool runtime and capability runtime must agree; the shared 'lean parameter normally enforces this",
        );
        let key = SessionPoolKey::from_capabilities(caps, imports, import_profile);
        self.remember_seen_key(&key);
        let (session, hit) = {
            let mut inner = self.inner.borrow_mut();
            if let Some(entry) = inner.take_matching(&key) {
                self.bump_reused();
                (
                    LeanSession::from_environment_with_import_stats(caps, entry.environment, entry.import_stats)?,
                    true,
                )
            } else {
                let reason = self.miss_reason(&inner);
                drop(inner);
                self.bump_key_miss(reason);
                self.enforce_before_fresh_import(imports)?;
                let session = caps.session_with_profile(imports, import_profile, cancellation, progress)?;
                self.remember_import_stats(session.import_stats().clone());
                self.bump_imported();
                (session, false)
            }
        };
        tracing::debug!(target: "lean_rs", hit = hit, "lean_rs.host.pool.acquire.result");
        Ok(PooledSession {
            pool: self,
            key,
            session: Some(session),
        })
    }

    /// Snapshot the accumulated pool metrics.
    ///
    /// Counters never reset; subtract two snapshots to measure activity
    /// over an interval. See [`PoolStats`] for the field invariants
    /// (e.g. `imports_performed + reused == acquired`).
    #[must_use]
    pub fn stats(&self) -> PoolStats {
        self.stats.get()
    }

    /// Number of environments currently sitting on the free list.
    ///
    /// This is the count of warm imports available for the next
    /// [`Self::acquire`] without going through `Lean.importModules`.
    /// Explicit drains and cache hits both remove entries from this
    /// count; releases may add entries back up to [`Self::capacity`].
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner.borrow().free.len()
    }

    /// `true` iff [`Self::len`] is zero; every subsequent
    /// [`Self::acquire`] will perform a fresh import.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Configured hard upper bound on the free list.
    ///
    /// Set by [`Self::with_capacity`]. A pool releasing a
    /// [`PooledSession`] while at capacity drops the environment
    /// instead of pushing it back; that release shows up in
    /// [`PoolStats::released_dropped`] rather than `released_to_pool`.
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Return the configured memory policy.
    #[must_use]
    pub fn memory_policy(&self) -> &SessionPoolMemoryPolicy {
        &self.memory_policy
    }

    /// Drop every cached environment currently retained by the pool.
    ///
    /// Returns the number of free-list entries removed. Each removed
    /// entry drops its owned `Obj<'lean>` environment, which releases
    /// one Lean refcount via `lean_dec`.
    ///
    /// Checked-out [`PooledSession`] values are not affected: they own
    /// their sessions until drop, and may return their environments to
    /// this same pool later if capacity permits. A later [`Self::drain`]
    /// call can remove those returned entries.
    ///
    /// This is a cache-eviction API, not a runtime recycle API. It does
    /// not reset Lean's process-global runtime state, initialized module
    /// flags, interned names, compacted `.olean` regions, or allocator
    /// arenas, and should not be treated as an RSS reset.
    pub fn drain(&self) -> usize {
        let mut inner = self.inner.borrow_mut();
        let drained = inner.free.len();
        inner.free.clear();

        let mut s = self.stats.get();
        s.drains = s.drains.saturating_add(1);
        s.drained = s.drained.saturating_add(u64::try_from(drained).unwrap_or(u64::MAX));
        self.stats.set(s);

        tracing::debug!(
            target: "lean_rs",
            drained = drained,
            "lean_rs.host.pool.drain",
        );
        drained
    }

    fn bump_reused(&self) {
        let mut s = self.stats.get();
        s.reused = s.reused.saturating_add(1);
        s.acquired = s.acquired.saturating_add(1);
        s.key_hits = s.key_hits.saturating_add(1);
        s.fresh_imports_avoided = s.fresh_imports_avoided.saturating_add(1);
        s.last_miss_reason = None;
        self.stats.set(s);
    }

    fn bump_imported(&self) {
        let mut s = self.stats.get();
        s.imports_performed = s.imports_performed.saturating_add(1);
        s.acquired = s.acquired.saturating_add(1);
        self.stats.set(s);
    }

    fn remember_seen_key(&self, key: &SessionPoolKey) {
        let mut inner = self.inner.borrow_mut();
        if inner.seen_keys.iter().all(|seen| seen != key) {
            inner.seen_keys.push(key.clone());
            let mut s = self.stats.get();
            s.distinct_keys_seen = u64::try_from(inner.seen_keys.len()).unwrap_or(u64::MAX);
            self.stats.set(s);
        }
    }

    fn miss_reason(&self, inner: &PoolInner<'_>) -> SessionPoolKeyMissReason {
        if self.capacity == 0 {
            SessionPoolKeyMissReason::ReuseDisabled
        } else if inner.free.is_empty() {
            SessionPoolKeyMissReason::EmptyPool
        } else {
            SessionPoolKeyMissReason::NoMatchingKey
        }
    }

    fn bump_key_miss(&self, reason: SessionPoolKeyMissReason) {
        let mut s = self.stats.get();
        s.key_misses = s.key_misses.saturating_add(1);
        match reason {
            SessionPoolKeyMissReason::EmptyPool => {
                s.miss_empty_pool = s.miss_empty_pool.saturating_add(1);
            }
            SessionPoolKeyMissReason::ReuseDisabled => {
                s.miss_reuse_disabled = s.miss_reuse_disabled.saturating_add(1);
            }
            SessionPoolKeyMissReason::NoMatchingKey => {
                s.miss_no_matching_key = s.miss_no_matching_key.saturating_add(1);
            }
        }
        s.last_miss_reason = Some(reason);
        self.stats.set(s);
    }

    fn bump_fresh_import_refusal(&self) {
        let mut s = self.stats.get();
        s.fresh_import_refusals = s.fresh_import_refusals.saturating_add(1);
        self.stats.set(s);
    }

    fn bump_rss_sample(&self, unavailable: bool) {
        let mut s = self.stats.get();
        s.rss_samples = s.rss_samples.saturating_add(1);
        if unavailable {
            s.rss_samples_unavailable = s.rss_samples_unavailable.saturating_add(1);
        }
        self.stats.set(s);
    }

    fn remember_import_stats(&self, stats: LeanImportStats) {
        *self.last_import_stats.borrow_mut() = Some(stats);
    }

    fn latest_import_stats_diagnostic(&self) -> String {
        self.last_import_stats.borrow().as_ref().map_or_else(
            || String::from("last_import_stats=unavailable"),
            |stats| format!("last_import_stats=available {}", stats.memory_diagnostic()),
        )
    }

    fn latest_import_stats_for_resource_facts(&self) -> Option<String> {
        self.last_import_stats
            .borrow()
            .as_ref()
            .map(LeanImportStats::memory_diagnostic)
    }

    fn resource_refusal(
        &self,
        cause: &str,
        message: String,
        current_rss_kib: Option<u64>,
        limit_kib: Option<u64>,
        import_count: Option<u64>,
        import_limit: Option<u64>,
        requested_imports: u64,
    ) -> LeanError {
        lean_rs::__host_internals::host_resource_exhausted_with_facts(
            message,
            ResourceExhaustedFacts {
                cause: cause.to_owned(),
                work_entered_lean: false,
                current_rss_kib,
                limit_kib,
                import_count,
                import_limit,
                requested_imports: Some(requested_imports),
                last_import_stats: self.latest_import_stats_for_resource_facts(),
            },
        )
    }

    fn enforce_before_fresh_import(&self, imports: &[&str]) -> LeanResult<()> {
        let stats = self.stats.get();
        if let Some(limit) = self.memory_policy.max_fresh_imports
            && stats.imports_performed >= limit
        {
            self.bump_fresh_import_refusal();
            return Err(self.resource_refusal(
                "same_process_fresh_import_limit",
                format!(
                "same-process SessionPool refused fresh import #{} for {} import(s): max_fresh_imports={limit}; {}; reuse a pooled environment or cycle the worker process",
                stats.imports_performed.saturating_add(1),
                imports.len(),
                self.latest_import_stats_diagnostic(),
                ),
                None,
                None,
                Some(stats.imports_performed),
                Some(limit),
                imports.len() as u64,
            ));
        }

        if let Some(limit_kib) = self.memory_policy.max_rss_kib {
            match current_process_rss_kib() {
                Some(current_kib) if current_kib >= limit_kib => {
                    self.bump_rss_sample(false);
                    self.bump_fresh_import_refusal();
                    return Err(self.resource_refusal(
                        "same_process_rss_ceiling",
                        format!(
                        "same-process SessionPool refused fresh import for {} import(s): current RSS {current_kib} KiB reached max_rss_kib={limit_kib}; {}; cycle the worker process to reset Lean process-global import state",
                        imports.len(),
                        self.latest_import_stats_diagnostic(),
                        ),
                        Some(current_kib),
                        Some(limit_kib),
                        Some(stats.imports_performed),
                        None,
                        imports.len() as u64,
                    ));
                }
                Some(_) => self.bump_rss_sample(false),
                None => {
                    self.bump_rss_sample(true);
                    self.bump_fresh_import_refusal();
                    return Err(self.resource_refusal(
                        "same_process_rss_sample_unavailable",
                        format!(
                        "same-process SessionPool refused fresh import for {} import(s): current RSS sample unavailable while max_rss_kib={limit_kib} is configured; {}",
                        imports.len(),
                        self.latest_import_stats_diagnostic(),
                        ),
                        None,
                        Some(limit_kib),
                        Some(stats.imports_performed),
                        None,
                        imports.len() as u64,
                    ));
                }
            }
        }

        Ok(())
    }

    fn release(&self, key: SessionPoolKey, env: Obj<'lean>, import_stats: LeanImportStats) {
        let mut inner = self.inner.borrow_mut();
        let mut s = self.stats.get();
        let kept = inner.free.len() < self.capacity;
        if kept {
            inner.free.push(PooledEntry {
                key,
                environment: env,
                import_stats,
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
            .field("memory_policy", &self.memory_policy)
            .field("len", &self.len())
            .field("stats", &self.stats.get())
            .finish()
    }
}

#[cfg(target_os = "linux")]
fn current_process_rss_kib() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    status.lines().find_map(|line| {
        let rest = line.strip_prefix("VmRSS:")?;
        rest.split_whitespace().next()?.parse::<u64>().ok()
    })
}

#[cfg(not(target_os = "linux"))]
fn current_process_rss_kib() -> Option<u64> {
    let output = Command::new("ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    text.trim().parse::<u64>().ok().filter(|value| *value > 0)
}

// -- PooledSession -------------------------------------------------------

/// A [`LeanSession`] borrowed from a [`SessionPool`].
///
/// Behaves as a [`LeanSession`] through [`core::ops::Deref`] /
/// [`core::ops::DerefMut`]—every session method is reachable directly:
///
/// ```ignore
/// let pool = lean_rs::SessionPool::with_capacity(runtime, 4);
/// let mut sess = pool.acquire(&caps, &["MyLib"], None, None)?;
/// let kind = sess.declaration_kind("MyLib.thing", None)?;
/// // dropping `sess` returns the imported environment to the pool
/// ```
///
/// On `Drop`, the underlying imported environment is returned to the
/// pool (or released if the pool is at capacity). Per-session
/// [`crate::host::session::SessionStats`] are scoped to the lifetime of
/// this checkout—they start at zero on every acquire and are
/// inaccessible after release.
///
/// Three lifetimes: `'lean` (runtime), `'p` (pool borrow), `'c`
/// (capability borrow). Neither [`Send`] nor [`Sync`] (inherited from
/// the contained [`LeanSession`]).
pub struct PooledSession<'lean, 'p, 'c> {
    pool: &'p SessionPool<'lean>,
    key: SessionPoolKey,
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
            let import_stats = session.import_stats().clone();
            let env = session.into_environment();
            self.pool.release(self.key.clone(), env, import_stats);
        }
    }
}

impl core::fmt::Debug for PooledSession<'_, '_, '_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("PooledSession").finish_non_exhaustive()
    }
}
