//! The child answers a repeated `OpenHostSession` without importing again.
//!
//! This is a memory contract before it is a latency one. Opening a session
//! runs a full `importModules`, and with `loadExts := true`
//! `Environment.freeRegions` is unsound, so imported regions are not
//! reclaimable even after the environment is dropped. A child that re-imports
//! on every open therefore grows with the *number of requests* rather than
//! with the workload — which is what every RSS ceiling above it was containing.
//!
//! The child therefore holds a bounded **pool** of imported environments rather
//! than one, so a workload alternating import profiles imports once per distinct
//! profile instead of once per switch. Holding an extra environment is cheap
//! next to importing one: measured over this fixture with four distinct import
//! sets, holding all four costs 4.19 GB against 4.04 GB for holding one after
//! the same four imports, while the alternating workload that avoids costs
//! 7.9 GB and sixteen imports.
//!
//! The tests here pin every half of that: an identical reopen does not import,
//! a switch back to a *pooled* set does not import, a switch to an unheld set
//! does, capacity 1 reproduces the pre-pool child exactly, and no reused session
//! ever serves an answer from the wrong environment.

#![allow(clippy::expect_used, clippy::panic, clippy::wildcard_enum_match_arm)]

use std::path::{Path, PathBuf};

use lean_rs_worker_parent::{
    LeanWorker, LeanWorkerConfig, LeanWorkerElabOptions, LeanWorkerModuleCacheStatus, LeanWorkerModuleQueryBatchItem,
    LeanWorkerModuleQueryBatchOutcome, LeanWorkerModuleQueryBatchResult, LeanWorkerModuleQueryCacheFacts,
    LeanWorkerModuleQuerySelector, LeanWorkerOutputBudgets, LeanWorkerRestartPolicy, LeanWorkerSessionConfig,
};

fn worker_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_lean-rs-worker-child"))
}

fn workspace_root() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .parent()
        .and_then(Path::parent)
        .expect("crates/<name> lives two directories below the workspace root")
        .to_path_buf()
}

fn fixture_root() -> PathBuf {
    workspace_root().join("fixtures").join("lean")
}

fn ensure_fixture_built() {
    lean_toolchain::build_lake_target_quiet(&fixture_root(), "LeanRsFixture").expect("fixture Lake target builds");
}

/// `LeanRsFixtureLateExtension` is deliberately its own `lean_lib`, outside the
/// `LeanRsFixture` roll-up, so it is reachable only through
/// `Lean.importModules`. Loading it as part of a capability dylib would run its
/// `initialize` block after `lean_io_mark_end_initialization`, where
/// `registerEnvExtension` throws. See the module docstring in the fixture.
fn ensure_late_extension_fixture_built() {
    ensure_fixture_built();
    lean_toolchain::build_lake_target_quiet(&fixture_root(), "LeanRsFixtureLateExtension")
        .expect("late-extension Lake target builds");
}

/// The RSS guard is disabled so the snapshot-cache assertions describe reuse
/// rather than whatever this machine's memory happens to be.
fn worker_config() -> LeanWorkerConfig {
    LeanWorkerConfig::new(worker_binary()).env("LEAN_RS_MODULE_CACHE_RSS_GUARD_KIB", "0")
}

fn elaboration_session_config() -> LeanWorkerSessionConfig {
    LeanWorkerSessionConfig::new(
        fixture_root(),
        "lean_rs_fixture",
        "LeanRsFixture",
        ["LeanRsHostShims.Elaboration"],
    )
}

fn handles_session_config() -> LeanWorkerSessionConfig {
    LeanWorkerSessionConfig::new(
        fixture_root(),
        "lean_rs_fixture",
        "LeanRsFixture",
        ["LeanRsHostShims.Elaboration", "LeanRsFixture.Handles"],
    )
}

fn cache_probe_selectors() -> Vec<LeanWorkerModuleQuerySelector> {
    vec![LeanWorkerModuleQuerySelector::ProofState {
        id: "state".to_owned(),
        line: 2,
        column: 4,
    }]
}

const PROBE_SOURCE: &str = "theorem t (h : True) : True := by\n  exact h\n";

fn batch_facts(outcome: &LeanWorkerModuleQueryBatchOutcome) -> &LeanWorkerModuleQueryCacheFacts {
    match outcome {
        LeanWorkerModuleQueryBatchOutcome::Ok { facts, .. }
        | LeanWorkerModuleQueryBatchOutcome::MissingImports { facts, .. }
        | LeanWorkerModuleQueryBatchOutcome::HeaderParseFailed { facts, .. } => facts,
        _ => panic!("batch cache facts unavailable on outcome {outcome:?}"),
    }
}

fn assert_batch_has_state(outcome: &LeanWorkerModuleQueryBatchOutcome) {
    let LeanWorkerModuleQueryBatchOutcome::Ok { result, .. } = outcome else {
        panic!("expected Ok batch outcome, got {outcome:?}");
    };
    assert!(
        result.items.iter().any(|item| matches!(
            item,
            LeanWorkerModuleQueryBatchItem::Ok { id, result }
                if id == "state" && matches!(result.as_ref(), LeanWorkerModuleQueryBatchResult::ProofState(_))
        )),
        "expected proof-state selector item, got {:?}",
        result.items,
    );
}

/// Populate the snapshot cache and return the resulting cache status.
fn probe_cache(worker: &mut LeanWorker, config: &LeanWorkerSessionConfig, label: &str) -> LeanWorkerModuleCacheStatus {
    let opts = LeanWorkerElabOptions::new().file_label(label);
    let mut session = worker.open_session(config, None, None).expect("worker session opens");
    let outcome = session
        .process_module_query_batch(
            PROBE_SOURCE,
            &cache_probe_selectors(),
            &LeanWorkerOutputBudgets::default(),
            &opts,
            None,
            None,
        )
        .expect("module query batch succeeds");
    assert_batch_has_state(&outcome);
    batch_facts(&outcome).cache_status
}

#[test]
fn reopening_an_identical_session_does_not_import_again() {
    ensure_fixture_built();
    let config = elaboration_session_config();
    let mut worker = LeanWorker::spawn(&worker_config()).expect("worker starts");

    {
        let _first = worker.open_session(&config, None, None).expect("first open");
    }
    assert_eq!(worker.stats().imports, 1, "the first open must import");

    {
        let _second = worker.open_session(&config, None, None).expect("second open");
    }
    assert_eq!(
        worker.stats().imports,
        1,
        "an identical reopen must reuse the live session; a second import here is unreclaimable memory \
         that grows with the request count"
    );
    // The request still crossed the wire — the child's answer is what decides,
    // so the parent must not be skipping the round trip.
    assert_eq!(worker.stats().requests, 2);

    let exit = worker.shutdown().expect("worker terminates").exit;
    assert!(exit.success);
}

#[test]
fn the_snapshot_cache_survives_a_reuse() {
    ensure_fixture_built();
    let config = elaboration_session_config();
    let mut worker = LeanWorker::spawn(&worker_config()).expect("worker starts");

    assert_eq!(
        probe_cache(&mut worker, &config, "/reuse/cache.lean"),
        LeanWorkerModuleCacheStatus::Miss
    );
    // The cached snapshot pins the environment of the session that built it,
    // so it may only outlive an open that kept that environment alive. This
    // one did, which is the whole point of not clearing on reuse.
    assert_eq!(
        probe_cache(&mut worker, &config, "/reuse/cache.lean"),
        LeanWorkerModuleCacheStatus::Hit,
        "reopening the same session must leave the retained snapshot reachable"
    );
    assert_eq!(worker.stats().imports, 1);

    let exit = worker.shutdown().expect("worker terminates").exit;
    assert!(exit.success);
}

#[test]
fn switching_back_to_a_pooled_import_set_does_not_reimport() {
    ensure_fixture_built();
    let narrow = elaboration_session_config();
    let wide = handles_session_config();
    let mut worker = LeanWorker::spawn(&worker_config()).expect("worker starts");

    // Distinct file labels, because that is what the workload looks like: a
    // call's imports are derived from its own file's header, so one file belongs
    // to one import profile. Snapshot entries are per-file, which is why the
    // pool holding several environments does not multiply them.
    assert_eq!(
        probe_cache(&mut worker, &narrow, "/switch/narrow.lean"),
        LeanWorkerModuleCacheStatus::Miss
    );
    assert_eq!(
        probe_cache(&mut worker, &wide, "/switch/wide.lean"),
        LeanWorkerModuleCacheStatus::Miss
    );
    assert_eq!(worker.stats().imports, 2);

    // A,B,A: the child parked A rather than dropping it, so going back is a
    // pool hit and not a third import. Holding the extra environment costs ~4%
    // of an import; re-importing costs all of one, and the residue is never
    // reclaimed.
    assert_eq!(
        probe_cache(&mut worker, &narrow, "/switch/narrow.lean"),
        LeanWorkerModuleCacheStatus::Hit,
        "the snapshot survived because the session that pinned its environment did — under the pre-pool child \
         the switch away cleared it"
    );
    assert_eq!(
        worker.stats().imports,
        2,
        "switching back to a pooled import set must not import a third time"
    );

    let exit = worker.shutdown().expect("worker terminates").exit;
    assert!(exit.success);
}

/// Two live environments must never answer for one another.
///
/// This is what stopped being covered for free when the pool removed the
/// unconditional clear on every switch: with both sessions alive and the same
/// file queried under each, only the module-query cache key separates them.
///
/// The two here differ **only** in [`HostSessionMode`] — same project, same
/// imports, same profile — which is the case `mode` was added to the key for.
/// Without it the second probe hits and answers from the first environment.
#[test]
fn a_pooled_sibling_in_another_mode_never_answers_from_the_wrong_environment() {
    ensure_fixture_built();
    let capability = elaboration_session_config();
    let shims_only = LeanWorkerSessionConfig::shims_only(fixture_root(), ["LeanRsHostShims.Elaboration"]);
    let mut worker = LeanWorker::spawn(&worker_config()).expect("worker starts");

    assert_eq!(
        probe_cache(&mut worker, &capability, "/shared/cache.lean"),
        LeanWorkerModuleCacheStatus::Miss
    );
    assert_eq!(
        probe_cache(&mut worker, &shims_only, "/shared/cache.lean"),
        LeanWorkerModuleCacheStatus::Rebuilt,
        "the same file under a different session mode must be recomputed, not served from the sibling's snapshot"
    );
    // Each environment keeps its own entry for the file, so going back is that
    // session's own hit rather than the sibling's answer.
    assert_eq!(
        probe_cache(&mut worker, &capability, "/shared/cache.lean"),
        LeanWorkerModuleCacheStatus::Hit
    );
    assert_eq!(
        worker.stats().imports,
        2,
        "one import per distinct environment; the switches were pool hits"
    );

    let exit = worker.shutdown().expect("worker terminates").exit;
    assert!(exit.success);
}

/// `LEAN_RS_WORKER_SESSION_POOL_CAPACITY=1` is the rollback lever, so it is
/// asserted rather than merely documented: at capacity 1 nothing is parked and
/// the child behaves exactly as it did before the pool existed.
#[test]
fn capacity_one_restores_the_pre_pool_child() {
    ensure_fixture_built();
    let narrow = elaboration_session_config();
    let wide = handles_session_config();
    let mut worker =
        LeanWorker::spawn(&worker_config().env("LEAN_RS_WORKER_SESSION_POOL_CAPACITY", "1")).expect("worker starts");

    assert_eq!(
        probe_cache(&mut worker, &narrow, "/capacity-one/cache.lean"),
        LeanWorkerModuleCacheStatus::Miss
    );
    assert_eq!(
        probe_cache(&mut worker, &wide, "/capacity-one/cache.lean"),
        LeanWorkerModuleCacheStatus::Miss
    );
    assert_eq!(
        probe_cache(&mut worker, &narrow, "/capacity-one/cache.lean"),
        LeanWorkerModuleCacheStatus::Miss,
        "capacity 1 parks nothing, so going back must re-import and lose the snapshot"
    );
    assert_eq!(worker.stats().imports, 3);
    // An identical reopen is still a reuse of the *current* session — that path
    // predates the pool and must not have been folded into it.
    assert_eq!(
        probe_cache(&mut worker, &narrow, "/capacity-one/cache.lean"),
        LeanWorkerModuleCacheStatus::Hit
    );
    assert_eq!(worker.stats().imports, 3);

    let exit = worker.shutdown().expect("worker terminates").exit;
    assert!(exit.success);
}

/// An alternation that fits the pool never evicts at all.
///
/// Named for what it asserts. It cannot say anything about *which* entry a full
/// pool evicts, because at capacity 2 an A,B,A,B workload never fills the pool:
/// the return to A is always a hit, under any eviction policy. See
/// [`a_full_pool_evicts_least_recently_used_and_re_imports`] for the test that
/// distinguishes them.
#[test]
fn an_alternation_that_fits_the_pool_never_evicts() {
    ensure_fixture_built();
    let narrow = elaboration_session_config();
    let wide = handles_session_config();
    // Capacity 2: current + one parked. A,B,A,B alternation fits exactly, so
    // nothing is ever evicted; a third distinct set would push A out.
    let mut worker =
        LeanWorker::spawn(&worker_config().env("LEAN_RS_WORKER_SESSION_POOL_CAPACITY", "2")).expect("worker starts");

    for _ in 0..3 {
        let _narrow = worker.open_session(&narrow, None, None).expect("narrow opens");
        let _wide = worker.open_session(&wide, None, None).expect("wide opens");
    }
    assert_eq!(
        worker.stats().imports,
        2,
        "an alternation that fits the pool must import once per distinct import set, not once per switch"
    );

    let exit = worker.shutdown().expect("worker terminates").exit;
    assert!(exit.success);
}

/// The bound `max_imports` exists to enforce cannot be silently skipped when
/// the parent's reuse prediction is wrong.
///
/// With the child pinned to capacity 1 and the parent still remembering both
/// keys, an A,B,A workload mispredicts the third open: the parent skips the
/// `max_imports` admission check, the child imports anyway, and the limit has
/// been crossed with no check having run. The deferred restart is what keeps
/// that bounded at exactly one import over the limit.
#[test]
fn a_mispredicted_reuse_restarts_before_the_next_import() {
    ensure_fixture_built();
    let narrow = elaboration_session_config();
    let wide = handles_session_config();
    let mut worker = LeanWorker::spawn(
        &worker_config()
            .env("LEAN_RS_WORKER_SESSION_POOL_CAPACITY", "1")
            .restart_policy(LeanWorkerRestartPolicy::default().max_imports(2)),
    )
    .expect("worker starts");

    {
        let _a = worker.open_session(&narrow, None, None).expect("A opens");
    }
    {
        let _b = worker.open_session(&wide, None, None).expect("B opens");
    }
    assert_eq!(worker.stats().restarts, 0, "the limit is not reached yet");

    {
        // Predicted a reuse (the parent remembers A), got an import: the child
        // parks nothing at capacity 1.
        let _a = worker.open_session(&narrow, None, None).expect("A reopens");
    }
    assert_eq!(
        worker.stats().restarts,
        0,
        "the restart is deferred, not taken out from under the session this call just opened"
    );

    {
        let _b = worker.open_session(&wide, None, None).expect("B reopens");
    }
    assert!(
        worker.stats().restarts >= 1,
        "the deferred restart must be taken before the next import, or max_imports never fires at all"
    );

    let exit = worker.shutdown().expect("worker terminates").exit;
    assert!(exit.success);
}

#[test]
fn a_reused_session_carries_no_state_from_the_previous_one() {
    ensure_fixture_built();
    let config = elaboration_session_config();
    let mut worker = LeanWorker::spawn(&worker_config()).expect("worker starts");

    {
        // A module query elaborates a whole source against the session's
        // environment. If any query path assigned back to that environment,
        // these two declarations — one plain, one `sorry`-bearing — would
        // survive into the next request, and reuse would be unsound.
        let mut session = worker.open_session(&config, None, None).expect("first open");
        let outcome = session
            .process_module_query_batch(
                "theorem reuseProbeDecl : True := trivial\ntheorem reuseProbeSorry : True := by\n  sorry\n",
                &cache_probe_selectors(),
                &LeanWorkerOutputBudgets::default(),
                &LeanWorkerElabOptions::new().file_label("/reuse/state.lean"),
                None,
                None,
            )
            .expect("module query batch succeeds");
        assert!(
            matches!(outcome, LeanWorkerModuleQueryBatchOutcome::Ok { .. }),
            "probe source should elaborate: {outcome:?}"
        );
    }

    {
        let mut session = worker.open_session(&config, None, None).expect("reopen reuses");
        for name in ["reuseProbeDecl", "reuseProbeSorry"] {
            let found = session.describe(name, None, None).expect("describe completes");
            assert!(
                found.is_none(),
                "reuse must not expose a declaration elaborated through the session: {name} resolved to {found:?}"
            );
        }
    }
    assert_eq!(worker.stats().imports, 1, "the reopen above must have been a reuse");

    // The same question across a *pooled* switch, which is what the pool makes
    // newly reachable: two environments are now concurrently live and
    // concurrently queried. The hazard is not module initializers — those are
    // correctly once-per-process, and skipping the second run is *required*,
    // because what an initializer produces is a global registry entry that each
    // environment's own `finalizePersistentExtensions` seeds from. The hazard
    // is that `Environment.extensions` is sized once at import, so an
    // environment can be left behind by a later registration; that is
    // `evict_stale_host_sessions`' job, covered by the staleness tests below.
    // What *this* test pins is the orthogonal question: nothing elaborated
    // through one session may be visible from the other, in either direction.
    let wide = handles_session_config();
    {
        let mut session = worker.open_session(&wide, None, None).expect("switch opens");
        for name in ["reuseProbeDecl", "reuseProbeSorry"] {
            let found = session.describe(name, None, None).expect("describe completes");
            assert!(
                found.is_none(),
                "a pooled sibling must not see a declaration elaborated through another session: {name}"
            );
        }
    }
    {
        let mut session = worker
            .open_session(&config, None, None)
            .expect("switch back is a pool hit");
        for name in ["reuseProbeDecl", "reuseProbeSorry"] {
            let found = session.describe(name, None, None).expect("describe completes");
            assert!(
                found.is_none(),
                "a parked session must not have accumulated state: {name}"
            );
        }
    }
    assert_eq!(
        worker.stats().imports,
        2,
        "the switch back must have been served from the pool"
    );

    let exit = worker.shutdown().expect("worker terminates").exit;
    assert!(exit.success);
}

#[test]
fn max_imports_does_not_restart_on_an_identical_reopen() {
    ensure_fixture_built();
    let config = elaboration_session_config();
    let mut worker =
        LeanWorker::spawn(&worker_config().restart_policy(LeanWorkerRestartPolicy::default().max_imports(1)))
            .expect("worker starts");

    {
        let _first = worker.open_session(&config, None, None).expect("first open");
    }
    {
        let _second = worker.open_session(&config, None, None).expect("second open");
    }

    let stats = worker.stats();
    assert_eq!(
        stats.restarts, 0,
        "restarting the child to bound imports, immediately before the open that would not have imported, \
         destroys the reuse it exists to make unnecessary"
    );
    assert_eq!(stats.imports, 1);

    let exit = worker.shutdown().expect("worker terminates").exit;
    assert!(exit.success);
}

/// The memory statement the rest of this file only implies: repeated identical
/// opens do not grow the child.
///
/// The bound is self-calibrating rather than a constant. The first open's own
/// RSS delta *is* the cost of one import on this machine and this fixture, so
/// asserting that eleven further reopens together cost less than that says
/// exactly "none of them imported" without hard-coding a number that would rot
/// with the fixture. Before session reuse this grew by eleven imports.
///
/// `#[ignore]` because it reads RSS, which off Linux means forking `ps` per
/// sample: correct, but too machine-dependent for a routine run.
#[test]
#[ignore = "measures child RSS; run explicitly"]
fn repeated_identical_opens_do_not_grow_the_child() {
    ensure_fixture_built();
    let config = elaboration_session_config();
    // A ceiling far above anything this fixture reaches: it never fires, and
    // setting it is what makes the parent sample child RSS on every request.
    let mut worker = LeanWorker::spawn(
        &worker_config().restart_policy(LeanWorkerRestartPolicy::default().max_rss_kib(64 * 1024 * 1024)),
    )
    .expect("worker starts");

    let sample = |worker: &LeanWorker| worker.stats().last_rss_kib.expect("RSS sample available");

    {
        let _open = worker.open_session(&config, None, None).expect("first open");
    }
    let before_import = sample(&worker);
    {
        let _open = worker.open_session(&config, None, None).expect("second open");
    }
    let after_one_import = sample(&worker);
    let one_import_kib = after_one_import.saturating_sub(before_import);
    assert!(
        one_import_kib > 0,
        "the first open must show a measurable import cost, or the bound below is vacuous"
    );

    for _ in 0..11 {
        let _open = worker.open_session(&config, None, None).expect("identical reopen");
    }
    let after_reopens = sample(&worker);
    let growth_kib = after_reopens.saturating_sub(after_one_import);

    // Asserted before the import count, so a regression here reports as the
    // memory claim this test exists for rather than as a duplicate of
    // `reopening_an_identical_session_does_not_import_again`.
    assert!(
        growth_kib < one_import_kib,
        "eleven identical reopens grew the child by {growth_kib} KiB, more than the {one_import_kib} KiB \
         one import costs — they are still importing"
    );
    assert_eq!(worker.stats().imports, 1);

    let exit = worker.shutdown().expect("worker terminates").exit;
    assert!(exit.success);
}

/// Alternating two pooled profiles costs less than one import, total.
///
/// The pooled counterpart to the test above, and the claim the session pool
/// exists to make: once both profiles are imported, switching between them is a
/// key comparison, so twenty-two further switches must together cost less than
/// the single import whose delta this measures first. Before the pool each
/// switch dropped the outgoing environment and re-imported — twenty-two
/// imports' worth of unreclaimable regions, since dropping reclaims essentially
/// nothing.
///
/// Every sample is taken after a settling pause and a further request, because
/// an import's pages keep materializing into RSS well after the open that
/// caused them returns: sampled immediately, the first import here reads as
/// 50 MB and the next twenty opens appear to grow the child by 2 GB they did
/// not allocate. `last_rss_kib` only refreshes on a request, hence the extra
/// reopen — itself a reuse, so it costs nothing being measured.
///
/// Self-calibrating and `#[ignore]`d for the same reasons as
/// [`repeated_identical_opens_do_not_grow_the_child`].
#[test]
#[ignore = "measures child RSS; run explicitly"]
fn alternating_two_pooled_profiles_does_not_grow_the_child() {
    ensure_fixture_built();
    let first = elaboration_session_config();
    let second = handles_session_config();
    let mut worker = LeanWorker::spawn(
        &worker_config().restart_policy(LeanWorkerRestartPolicy::default().max_rss_kib(64 * 1024 * 1024)),
    )
    .expect("worker starts");

    // Sample after letting the previous open's pages settle, then re-request so
    // the parent actually refreshes its reading.
    let settled_rss = |worker: &mut LeanWorker, config: &LeanWorkerSessionConfig| {
        std::thread::sleep(std::time::Duration::from_millis(750));
        {
            let _settling = worker.open_session(config, None, None).expect("settling reopen");
        }
        worker.stats().last_rss_kib.expect("RSS sample available")
    };

    {
        let _open = worker.open_session(&first, None, None).expect("first profile imports");
    }
    let after_one_import = settled_rss(&mut worker, &first);
    {
        let _open = worker
            .open_session(&second, None, None)
            .expect("second profile imports");
    }
    let after_two_imports = settled_rss(&mut worker, &second);
    let one_import_kib = after_two_imports.saturating_sub(after_one_import);
    assert!(
        one_import_kib > 0,
        "the second profile's import must show a measurable cost, or the bound below is vacuous"
    );

    for _ in 0..11 {
        {
            let _back = worker.open_session(&first, None, None).expect("switch back");
        }
        let _forward = worker.open_session(&second, None, None).expect("switch forward");
    }
    let after_switches = settled_rss(&mut worker, &second);
    let growth_kib = after_switches.saturating_sub(after_two_imports);

    assert!(
        growth_kib < one_import_kib,
        "twenty-two profile switches grew the child by {growth_kib} KiB, more than the {one_import_kib} KiB \
         one import costs — the pool is not holding them"
    );
    assert_eq!(
        worker.stats().imports,
        2,
        "two distinct profiles must cost exactly two imports, however often they alternate"
    );

    let exit = worker.shutdown().expect("worker terminates").exit;
    assert!(exit.success);
}

// -- staleness: an environment left behind by a later registration -------
//
// `Environment.extensions` is sized exactly once, by `mkInitialExtensionStates`
// inside `finalizeImport`. The only growth path is `private`, as is the field,
// so an environment that outlives a `registerEnvExtension` can never be
// repaired. Registration during import is ordinary, not exotic: `initializing`
// is true for all of `importModules`, which is how every user `initialize`
// block and every `register_simp_attr` comes into existence.
//
// The damage is unfiltered global iteration. `ScopedEnvExtension.pushScope`,
// `popScope`, `setDelimitsLocal`, and `activateScoped` walk the process-global
// `scopedEnvExtensionsRef` *without* filtering against the environment they are
// modifying, so each out-of-range slot reaches
// `panic! "invalid environment extension has been accessed"`. Their callers are
// ordinary elaboration: every `namespace`, every `section`, every `open … in`.
//
// And it is a hang, not noise. The parent pipes child stderr but drains it only
// after the child exits, so once the panic text fills the pipe buffer the child
// blocks in `write(2)` and stops answering the protocol. One late extension
// means one line per scope command — far below pipe capacity — so the
// reproducer below cannot itself wedge.

const SCOPE_SOURCE: &str =
    "namespace P\n\nsection S\n\nopen Nat in\ntheorem t (h : True) : True := by\n  exact h\n\nend S\n\nend P\n";

const EXTENSION_PANIC: &str = "invalid environment extension has been accessed";

fn staleness_probe_config() -> LeanWorkerSessionConfig {
    LeanWorkerSessionConfig::shims_only(fixture_root(), ["LeanRsHostShims.Elaboration"])
}

/// Imports the module whose `initialize` block registers a scoped environment
/// extension. Shims-only, so the registration happens inside
/// `Lean.importModules` rather than at dylib load, where it would throw.
fn staleness_registrar_config() -> LeanWorkerSessionConfig {
    LeanWorkerSessionConfig::shims_only(
        fixture_root(),
        ["LeanRsHostShims.Elaboration", "LeanRsFixtureLateExtension"],
    )
}

/// The control: a distinct import profile that registers nothing.
fn staleness_inert_config() -> LeanWorkerSessionConfig {
    LeanWorkerSessionConfig::shims_only(fixture_root(), ["LeanRsHostShims.Elaboration", "LeanRsFixture.Meta"])
}

/// Elaborate a source containing every command that reaches the unfiltered
/// walk: `namespace`, `section`, and `open … in`.
fn probe_scopes(
    worker: &mut LeanWorker,
    config: &LeanWorkerSessionConfig,
    label: &str,
) -> LeanWorkerModuleQueryBatchOutcome {
    let mut session = worker.open_session(config, None, None).expect("worker session opens");
    let outcome = session
        .process_module_query_batch(
            SCOPE_SOURCE,
            &[LeanWorkerModuleQuerySelector::ProofState {
                id: "state".to_owned(),
                line: 7,
                column: 4,
            }],
            &LeanWorkerOutputBudgets::default(),
            &LeanWorkerElabOptions::new().file_label(label),
            None,
            None,
        )
        .expect("module query batch succeeds");
    assert_batch_has_state(&outcome);
    outcome
}

/// The reproducer. Fails on a child that reuses a pooled environment across a
/// registering import.
#[test]
fn a_late_extension_registration_never_reaches_a_pooled_environment() {
    ensure_late_extension_fixture_built();
    let probe = staleness_probe_config();
    let mut worker = LeanWorker::spawn(&worker_config()).expect("worker starts");

    probe_scopes(&mut worker, &probe, "/staleness/probe-first.lean");
    {
        let _registrar = worker
            .open_session(&staleness_registrar_config(), None, None)
            .expect("registering import succeeds");
    }
    // A *different* file label, so this actually elaborates. Reusing the first
    // label would hit the module snapshot cache, which answers without running
    // a single command and makes the reproducer vacuous — that is precisely
    // how this test first passed against a child known to be broken.
    //
    // The `Ok`-outcome assertion inside `probe_scopes` is load-bearing for the
    // same reason: a non-`Ok` outcome would satisfy the check below trivially.
    probe_scopes(&mut worker, &probe, "/staleness/probe-second.lean");

    // Not asserted on `exit.diagnostics`: the panicking generation dies and is
    // replaced, so its stderr never reaches the final shutdown's exit record.
    // The restart counter is the same evidence one level up, and it is the
    // production symptom exactly — a dead child, mid-request.
    let stats = worker.stats();
    assert_eq!(
        stats.restarts, 0,
        "the child died serving a query against a stale pooled environment (last reason: {:?})",
        stats.last_restart_reason,
    );
    assert_eq!(stats.exits, 0);

    let exit = worker.shutdown().expect("worker terminates").exit;
    assert!(exit.success);
    assert!(!exit.diagnostics.contains(EXTENSION_PANIC), "{}", exit.diagnostics);
}

/// The mechanism behind the reproducer: the pooled environment is *dropped*,
/// and the snapshot it pinned goes with it. An unfixed child reports `Hit` here
/// at two imports.
#[test]
fn a_late_extension_registration_evicts_the_pooled_environment() {
    ensure_late_extension_fixture_built();
    let probe = staleness_probe_config();
    let mut worker = LeanWorker::spawn(&worker_config()).expect("worker starts");

    assert_eq!(
        batch_facts(&probe_scopes(&mut worker, &probe, "/staleness/evict.lean")).cache_status,
        LeanWorkerModuleCacheStatus::Miss
    );
    {
        let _registrar = worker
            .open_session(&staleness_registrar_config(), None, None)
            .expect("registering import succeeds");
    }
    assert_eq!(
        batch_facts(&probe_scopes(&mut worker, &probe, "/staleness/evict.lean")).cache_status,
        LeanWorkerModuleCacheStatus::Miss,
        "the stale environment must be dropped, and the snapshot it pinned with it"
    );
    assert_eq!(
        worker.stats().imports,
        3,
        "probe, registrar, and the probe's forced re-import"
    );

    let exit = worker.shutdown().expect("worker terminates").exit;
    assert!(exit.success);
    assert!(!exit.diagnostics.contains(EXTENSION_PANIC), "{}", exit.diagnostics);
}

/// The negative. Without it the fix could degenerate into "evict on every
/// import", which would cost an import per profile switch and undo the pool.
#[test]
fn an_import_that_registers_nothing_leaves_the_pool_intact() {
    ensure_late_extension_fixture_built();
    let probe = staleness_probe_config();
    let mut worker = LeanWorker::spawn(&worker_config()).expect("worker starts");

    assert_eq!(
        batch_facts(&probe_scopes(&mut worker, &probe, "/staleness/inert.lean")).cache_status,
        LeanWorkerModuleCacheStatus::Miss
    );
    {
        let _inert = worker
            .open_session(&staleness_inert_config(), None, None)
            .expect("inert import succeeds");
    }
    assert_eq!(
        batch_facts(&probe_scopes(&mut worker, &probe, "/staleness/inert.lean")).cache_status,
        LeanWorkerModuleCacheStatus::Hit,
        "an import that registers nothing must leave the pooled environment and its snapshot alone"
    );
    assert_eq!(
        worker.stats().imports,
        2,
        "the switch back must have been served from the pool"
    );

    let exit = worker.shutdown().expect("worker terminates").exit;
    assert!(exit.success);
}

/// The rollback lever was never exposed to staleness and still is not: at
/// capacity 1 the registrar's open already displaced the probe environment, so
/// eviction has nothing left to do and the observable behaviour is unchanged.
#[test]
fn capacity_one_is_unaffected_by_staleness_eviction() {
    ensure_late_extension_fixture_built();
    let probe = staleness_probe_config();
    let mut worker =
        LeanWorker::spawn(&worker_config().env("LEAN_RS_WORKER_SESSION_POOL_CAPACITY", "1")).expect("worker starts");

    probe_scopes(&mut worker, &probe, "/staleness/capacity-one-first.lean");
    {
        let _registrar = worker
            .open_session(&staleness_registrar_config(), None, None)
            .expect("registering import succeeds");
    }
    probe_scopes(&mut worker, &probe, "/staleness/capacity-one-second.lean");
    assert_eq!(
        worker.stats().imports,
        3,
        "capacity 1 parks nothing, so this re-imports"
    );

    let exit = worker.shutdown().expect("worker terminates").exit;
    assert!(exit.success);
    assert!(!exit.diagnostics.contains(EXTENSION_PANIC), "{}", exit.diagnostics);
}

/// A staleness eviction must not cost the child its reuse hint.
///
/// The parent latches `reuse_hint_enabled` off when it predicts a reuse, gets an
/// import, and has never seen this child reuse anything — the signature of a
/// pre-reuse worker binary. A staleness eviction produces exactly that
/// signature on a perfectly healthy child, and the latch is for the whole
/// generation: every later open is then admitted as `import_like`, so the
/// `max_imports` bound counts pool *hits* and cycles the child on what should
/// have cost nothing. Any child whose first two profiles differ in modules would
/// trip it during warm-up.
///
/// The observable is the admission gate, not the restart counter: a request the
/// parent believes will reuse skips the gate entirely, so
/// `import_like_admission_attempts` is a direct read of what the hint believes.
#[test]
fn a_staleness_eviction_does_not_disable_the_reuse_hint() {
    ensure_late_extension_fixture_built();
    let probe = staleness_probe_config();
    let registrar = staleness_registrar_config();
    let mut worker = LeanWorker::spawn(&worker_config()).expect("worker starts");

    for (config, label) in [
        (&probe, "probe"),
        (&registrar, "registrar"),
        (&probe, "probe again, after the registration swept it"),
    ] {
        let _session = worker
            .open_session(config, None, None)
            .unwrap_or_else(|err| panic!("open ({label}) succeeds: {err}"));
    }
    assert_eq!(worker.stats().imports, 3);

    {
        let _session = worker
            .open_session(&registrar, None, None)
            .expect("the registrar's environment is still pooled");
    }
    let stats = worker.stats();
    assert_eq!(stats.imports, 3, "the fourth open must be a pool hit");
    assert_eq!(
        stats.import_like_admission_attempts, 2,
        "only the two opens of a never-before-seen key are import-like; the hint must have survived the \
         staleness eviction, or every later open pays the admission gate and the pool is dead on arrival"
    );
    assert_eq!(stats.restarts, 0);

    let exit = worker.shutdown().expect("worker terminates").exit;
    assert!(exit.success);
}

// -- capacity: the eviction path that used to be unreachable --------------
//
// Pool capacity used to be injected from the parent's `max_imports` restart
// bound, which made the two equal by construction — and so made the child's LRU
// eviction dead code, because the parent cycled the child at exactly the point
// the pool would have begun evicting. The two are now independent, sized by what
// they each actually cost: a held environment is ~70 MiB, an import is 2–4 GB of
// unreclaimable residue. These tests pin the eviction path now that it runs.

/// A third distinct pool key. Same closure as [`staleness_inert_config`] in a
/// different order, which is deliberate: the key is the ordered import list, so
/// this is a genuine pool miss while importing nothing the fixture has not
/// already proven safe to import.
fn reordered_inert_config() -> LeanWorkerSessionConfig {
    LeanWorkerSessionConfig::shims_only(fixture_root(), ["LeanRsFixture.Meta", "LeanRsHostShims.Elaboration"])
}

/// Open three distinct profiles, then the first again, and report how many
/// imports that cost. Four means the first was evicted; three means it was
/// still pooled.
fn imports_over_three_profiles_and_a_return(worker: &mut LeanWorker) -> u64 {
    for config in [
        staleness_probe_config(),
        staleness_inert_config(),
        reordered_inert_config(),
        staleness_probe_config(),
    ] {
        let _session = worker.open_session(&config, None, None).expect("open succeeds");
    }
    worker.stats().imports
}

/// A full pool evicts the **least recently used** entry and keeps the incoming
/// one. Pinned because the obvious alternative — `lean-rs-host`'s own
/// `SessionPool`, which drops the *incoming* entry at capacity — inverts it: the
/// same sequence under that policy keeps the first profile pooled and answers
/// the return in three imports rather than four.
#[test]
fn a_full_pool_evicts_least_recently_used_and_re_imports() {
    ensure_late_extension_fixture_built();
    // Capacity counts the session being made current, so the pool keeps one.
    let mut worker = LeanWorker::spawn(&worker_config().session_pool_capacity(2)).expect("worker starts");

    assert_eq!(
        imports_over_three_profiles_and_a_return(&mut worker),
        4,
        "the third profile must evict the first, so returning to it re-imports"
    );
    assert_eq!(worker.stats().restarts, 0, "capacity eviction is not a restart");

    let exit = worker.shutdown().expect("worker terminates").exit;
    assert!(exit.success);
}

#[test]
fn a_pool_with_room_serves_the_return_without_importing() {
    ensure_late_extension_fixture_built();
    let mut worker = LeanWorker::spawn(&worker_config().session_pool_capacity(4)).expect("worker starts");

    assert_eq!(
        imports_over_three_profiles_and_a_return(&mut worker),
        3,
        "with room for all three, the return is a key comparison and not an import"
    );

    let exit = worker.shutdown().expect("worker terminates").exit;
    assert!(exit.success);
}

/// Eviction discards the process-global snapshot cache, including entries built
/// by sessions that are still live. Pinned as a chosen cost rather than left to
/// be discovered: Lean's cache clears all-or-nothing, and a retained snapshot
/// pins the environment being dropped, so the clear is not optional.
#[test]
fn a_capacity_eviction_clears_the_snapshot_cache() {
    ensure_late_extension_fixture_built();
    let probe = staleness_probe_config();
    const LABEL: &str = "/capacity/evicted.lean";

    let mut evicting = LeanWorker::spawn(&worker_config().session_pool_capacity(2)).expect("worker starts");
    assert_eq!(
        batch_facts(&probe_scopes(&mut evicting, &probe, LABEL)).cache_status,
        LeanWorkerModuleCacheStatus::Miss
    );
    for config in [staleness_inert_config(), reordered_inert_config()] {
        let _session = evicting.open_session(&config, None, None).expect("open succeeds");
    }
    assert_eq!(
        batch_facts(&probe_scopes(&mut evicting, &probe, LABEL)).cache_status,
        LeanWorkerModuleCacheStatus::Miss,
        "the evicted environment's snapshot must go with it"
    );
    assert!(evicting.shutdown().expect("worker terminates").exit.success);

    // The control, so the assertion above is about eviction and not about
    // opening other sessions at all.
    let mut roomy = LeanWorker::spawn(&worker_config().session_pool_capacity(4)).expect("worker starts");
    assert_eq!(
        batch_facts(&probe_scopes(&mut roomy, &probe, LABEL)).cache_status,
        LeanWorkerModuleCacheStatus::Miss
    );
    for config in [staleness_inert_config(), reordered_inert_config()] {
        let _session = roomy.open_session(&config, None, None).expect("open succeeds");
    }
    assert_eq!(
        batch_facts(&probe_scopes(&mut roomy, &probe, LABEL)).cache_status,
        LeanWorkerModuleCacheStatus::Hit,
        "without an eviction there is nothing to clear"
    );
    assert!(roomy.shutdown().expect("worker terminates").exit.success);
}

/// The decoupling itself: a generous import bound must not buy pool capacity.
#[test]
fn pool_capacity_is_independent_of_the_import_restart_bound() {
    ensure_late_extension_fixture_built();
    let policy = LeanWorkerRestartPolicy::default().max_imports(16);

    let mut derived = LeanWorker::spawn(&worker_config().restart_policy(policy.clone())).expect("worker starts");
    assert_eq!(
        imports_over_three_profiles_and_a_return(&mut derived),
        3,
        "with no explicit capacity the bound still supplies one, and 16 is room enough"
    );
    assert!(derived.shutdown().expect("worker terminates").exit.success);

    let mut explicit =
        LeanWorker::spawn(&worker_config().restart_policy(policy).session_pool_capacity(2)).expect("worker starts");
    assert_eq!(
        imports_over_three_profiles_and_a_return(&mut explicit),
        4,
        "capacity 2 must bind even though the import bound is 16"
    );
    assert_eq!(explicit.stats().restarts, 0);
    assert!(explicit.shutdown().expect("worker terminates").exit.success);
}

/// The rollback lever stays reachable: an embedder that names the variable
/// outranks whatever the config resolved.
#[test]
fn an_explicit_child_environment_outranks_the_configured_capacity() {
    ensure_late_extension_fixture_built();
    let mut worker = LeanWorker::spawn(
        &worker_config()
            .env("LEAN_RS_WORKER_SESSION_POOL_CAPACITY", "1")
            .session_pool_capacity(4),
    )
    .expect("worker starts");

    assert_eq!(
        imports_over_three_profiles_and_a_return(&mut worker),
        4,
        "capacity 1 parks nothing, so every switch re-imports"
    );

    let exit = worker.shutdown().expect("worker terminates").exit;
    assert!(exit.success);
}
