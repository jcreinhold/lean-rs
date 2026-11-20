"""Provision or qualify the reviewed ordinary Lean 4.36 worker fixture."""

import argparse
import fcntl
import hashlib
import io
import json
import os
import platform
import runpy
import shutil
import stat
import subprocess
import sys
import tomllib
from contextlib import contextmanager, redirect_stdout
from pathlib import Path

SDK = Path(__file__).resolve().parents[1]
FIXTURE_SDK = "945792112a180bd9e3a90194159e0c9c158cde2d"
APP_TREE = "e50fe473928dede19987ab655000f80a8c5a74cd"
TYPED_APP_TREE = "9a37c723ce30cce1f8a63c5206a377b5e7632d7b"
# Metadata deployments sealed before the typed policy used this revision of the same app.
ORIGINAL_APP_TREE = "ee1bf0cecda10560c7b58b33ea679188f87fea9c"
TYPED_SDK = "9914894988ec3bfd123f1d10b407c066eb7c6888"
LAKE_SHA256 = "a0e516de0ba03a26cb90c80f06666022ef9de3f0943f8b26e9b902d229b43ebe"
TESTS = (
    "builder_opens_worker_capability_and_validates_metadata",
    "metadata_mismatch_is_reported_by_bootstrap_check",
)
APP = SDK / "fixtures/metadata-qualification/app"
TARGET = SDK / "target"
STATE = TARGET / "metadata-qualification/ordinary"
STATE_LOCK = TARGET / "metadata-qualification/.ordinary.lock"
SOCKET_ROOT = TARGET / "ws"
BROKEN_IMPORT_TEST = "a_host_handle_built_with_a_broken_import_still_fails_at_open"
TRANSITIVE_WORKER_TEST = "shims_only_host_handle_imports_transitive_lake_package_oleans"
DIRECT_TRANSITIVE_TEST = "import_finds_transitive_lake_package_oleans"
GOOD_WORKER_TEST = "shims_only_session_opens_without_user_dylib"
DIRECT_GOOD_TEST = "load_shims_only_opens_session_against_user_oleans"
DIRECT_BROKEN_SHARED_TEST = (
    "load_shims_only_succeeds_when_user_shared_facet_does_not_build"
)
DIRECT_OPTIONAL_WHNF_TEST = "meta_missing_optional_symbol_returns_unsupported"
DIRECT_BUILDER_TESTS = {
    DIRECT_OPTIONAL_WHNF_TEST: (
        DIRECT_OPTIONAL_WHNF_TEST,
        b"direct-host-optional-whnf-passed",
    ),
    DIRECT_BROKEN_SHARED_TEST: (
        DIRECT_BROKEN_SHARED_TEST,
        b"direct-host-broken-shared-passed",
    ),
    TRANSITIVE_WORKER_TEST: (DIRECT_TRANSITIVE_TEST, b"direct-host-transitive-passed"),
    GOOD_WORKER_TEST: (DIRECT_GOOD_TEST, b"direct-host-good-passed"),
}
HOST_BUILDER_CASES = (
    (
        GOOD_WORKER_TEST,
        "--host-builder-good",
        "lean-good",
        ((b"authorize-builder-good",),),
    ),
    (
        "a_host_handle_built_without_imports_opens_no_session",
        "--host-builder-lazy",
        "lean-lazy",
        ((b"authorize-builder-good",), (b"authorize-builder-empty",)),
    ),
    (
        "shims_only_host_handle_skips_user_shared_build",
        "--host-builder-shims-only",
        "lean-shims-only",
        (
            (
                b"authorize-builder-good",
                b"authorize-builder-good",
                b"authorize-builder-broken",
            ),
        ),
    ),
    (
        TRANSITIVE_WORKER_TEST,
        "--host-builder-transitive",
        "lean-transitive",
        (
            (
                b"authorize-builder-consumer",
                b"authorize-builder-consumer",
                b"authorize-builder-missing-dependency",
            ),
        ),
    ),
)
ORDINARY_TYPED_TESTS = (
    "describe_distinguishes_known_and_missing_declarations",
    "search_declarations_applies_filters_ranks_and_limits",
    "core_declaration_queries_preserve_missing_slots_and_type_caps",
    "inspect_declaration_distinguishes_theorem_definition_and_missing",
    "inspect_declaration_bounds_output_and_opts_in_to_proof_search",
    "search_result_name_can_be_inspected",
    "list_declarations_strings_streams_full_env_without_frame_cap",
    "oversized_terminal_response_is_request_error_not_child_death",
    "inspect_declaration_truncates_and_distinguishes_renderings",
    "process_module_query_batch_reuses_snapshot_for_same_file_content",
    "process_module_query_batch_rebuilds_when_content_changes",
    "process_module_query_batch_incremental_rebuild_matches_a_cold_build",
    "process_module_query_batch_clear_evicts_snapshot",
    "process_module_query_batch_isolates_snapshots_by_session_imports",
    "process_module_query_batch_ttl_evicts_idle_snapshot",
    "process_module_query_batch_entry_limit_evicts_old_entries",
    "process_module_query_batch_tiny_cache_still_returns_correct_query",
)
ORDINARY_SESSION_REUSE_TESTS = (
    "reopening_an_identical_session_does_not_import_again",
    "max_imports_does_not_restart_on_an_identical_reopen",
    "a_full_pool_evicts_least_recently_used_and_re_imports",
    "a_pool_with_room_serves_the_return_without_importing",
    "pool_capacity_is_independent_of_the_import_restart_bound",
    "an_explicit_child_environment_outranks_the_configured_capacity",
)
ORDINARY_APP_TEST_COUNT = len(ORDINARY_TYPED_TESTS) + len(ORDINARY_SESSION_REUSE_TESTS)
# Allowed (started workers, manifest grants). A policy replacement can reach
# READY without opening a session. The Rust cases still check the policy result.
ORDINARY_JSON_TESTS = (
    ("pool", "compatible_session_key_reuses_one_worker", ((1, 1),)),
    ("typed_command", "typed_json_command_decodes_response", ((1, 1),)),
    ("pool", "import_profile_partitions_pool_session_keys", ((1, 1),)),
    ("pool", "import_workspace_root_partitions_pool_session_keys", ((1, 1),)),
    ("pool", "explicit_cycle_invalidates_only_the_current_lease", ((2, 2),)),
    ("pool", "queue_wait_timeout_is_typed_when_pool_is_full", ((1, 1),)),
    (
        "pool",
        "per_worker_rss_policy_invalidates_old_lease_before_work",
        ((1, 1), (2, 1)),
    ),
    # If the renewed child goes idle before reassignment, it is replaced again
    # before that intermediate child can receive a session.
    (
        "pool",
        "idle_policy_invalidates_lease_and_fresh_lease_can_continue",
        ((2, 2), (3, 2)),
    ),
    (
        "pool",
        "memory_budget_rejects_new_distinct_worker_when_known_rss_is_exhausted",
        ((1, 1), (2, 2)),
    ),
    (
        "pool",
        "rss_snapshot_records_available_samples_on_supported_platforms",
        ((2, 2),),
    ),
)
ORDINARY_META_TESTS = (
    ("typed_session", "infer_type_returns_rendered_type_for_known_term", ((1, 1),)),
    ("typed_session", "whnf_reduces_known_term", ((1, 1),)),
    ("typed_session", "is_def_eq_recognises_equal_terms", ((1, 2),)),
)
ADAPTER_IMPORT_GRANT = b"authorize-adapter-handles"
ORDINARY_ADAPTER_TESTS = (
    (
        "session_adapter",
        "declaration_bulk_adapter_matches_in_process_session",
        ((1, 1),),
    ),
    ("session_adapter", "progress_events_cross_worker_boundary_in_order", ((1, 1),)),
    (
        "session_adapter",
        "parent_can_cancel_long_worker_request_at_progress_boundary",
        ((2, 1),),
    ),
    ("supervisor", "max_import_policy_restarts_before_next_import", ((2, 2),)),
)
ORDINARY_ADAPTER_KERNEL_GRANT = b"authorize-adapter-elaboration"
ORDINARY_ADAPTER_KERNEL_TESTS = (
    ("session_adapter", "elaboration_and_kernel_adapter_match_status_shape", ((1, 1),)),
)
ORDINARY_ADAPTER_CASES = frozenset(
    name for _, name, _ in (*ORDINARY_ADAPTER_TESTS, *ORDINARY_ADAPTER_KERNEL_TESTS)
)
ORDINARY_MODULE_SYNTAX_CASE = (
    "process_module_query_handles_module_system_header_through_worker"
)
ORDINARY_MODULE_QUERY_TESTS = (
    ("typed_session", ORDINARY_MODULE_SYNTAX_CASE, ((1, 1),)),
    (
        "typed_session",
        "process_module_query_returns_diagnostics_through_worker",
        ((1, 1),),
    ),
    (
        "typed_session",
        "process_module_query_diagnostics_keep_original_source_coordinates",
        ((1, 1),),
    ),
    (
        "typed_session",
        "process_module_query_returns_cursor_type_and_goal_through_worker",
        ((1, 2),),
    ),
    ("typed_session", "process_module_query_references_through_worker", ((1, 1),)),
)
ORDINARY_PROOF_TESTS = (
    (
        "typed_session",
        "attempt_proof_successful_candidate_closes_simple_goal",
        ((1, 1),),
    ),
    (
        "typed_session",
        "attempt_proof_envelope_entry_state_carries_goals_and_locals",
        ((1, 1),),
    ),
    ("typed_session", "attempt_proof_bad_selector_yields_empty_entry_state", ((1, 1),)),
    (
        "typed_session",
        "attempt_proof_bad_candidate_on_unicode_signature_does_not_false_close",
        ((1, 1),),
    ),
    (
        "typed_session",
        "attempt_proof_single_line_by_tactic_aligns_and_closes",
        ((1, 1),),
    ),
    (
        "typed_session",
        "attempt_proof_entry_selector_accepts_from_scratch_block_where_default_traps",
        ((1, 2),),
    ),
    (
        "typed_session",
        "attempt_proof_candidate_list_is_capped_and_does_not_write_files",
        ((1, 1),),
    ),
    ("typed_session", "attempt_proof_ten_candidates_returns_ordered_rows", ((1, 1),)),
    (
        "typed_session",
        "attempt_proof_global_output_budget_returns_partial_not_attempted_rows",
        ((1, 1),),
    ),
)
ORDINARY_BATCH_TESTS = (
    (
        "typed_session",
        "process_module_query_batch_returns_diagnostics_and_proof_state_through_worker",
        ((1, 1),),
    ),
    (
        "typed_session",
        "declaration_outline_batch_selector_returns_source_order_rows_through_worker",
        ((1, 1),),
    ),
    (
        "typed_session",
        "proof_state_in_declaration_renders_locals_pretty_by_default_and_raw_on_opt_out",
        ((1, 2),),
    ),
    (
        "typed_session",
        "proof_state_unresolved_after_text_returns_boundary_candidates_through_worker",
        ((1, 1),),
    ),
    (
        "typed_session",
        "proof_state_boundary_candidates_report_truncation_through_worker",
        ((1, 1),),
    ),
    (
        "typed_session",
        "proof_state_entry_selector_renders_pristine_entry_goal",
        ((1, 1),),
    ),
    (
        "typed_session",
        "module_query_on_incomplete_closure_skips_body_elaboration",
        ((1, 1),),
    ),
    ("session_reuse", "the_snapshot_cache_survives_a_reuse", ((1, 2),)),
    (
        "session_reuse",
        "a_pooled_sibling_in_another_mode_never_answers_from_the_wrong_environment",
        ((1, 3),),
    ),
)
ORDINARY_REUSE_BATCH_TESTS = (
    (
        "session_reuse",
        "switching_back_to_a_pooled_import_set_does_not_reimport",
        ((1, 3),),
    ),
    ("session_reuse", "capacity_one_restores_the_pre_pool_child", ((1, 4),)),
    (
        "session_reuse",
        "a_reused_session_carries_no_state_from_the_previous_one",
        ((1, 1),),
    ),
    ("session_reuse", "a_capacity_eviction_clears_the_snapshot_cache", ((2, 4),)),
)
ORDINARY_REUSE_IMPORT_TESTS = (
    ("session_reuse", "an_alternation_that_fits_the_pool_never_evicts", ((1, 6),)),
    (
        "session_reuse",
        "a_mispredicted_reuse_restarts_before_the_actual_import",
        ((2, 5),),
    ),
)
ORDINARY_STALENESS_BATCH_TESTS = (
    (
        "session_reuse",
        "a_late_extension_registration_never_reaches_a_pooled_environment",
        ((1, 2),),
    ),
    (
        "session_reuse",
        "a_late_extension_registration_evicts_the_pooled_environment",
        ((1, 2),),
    ),
    (
        "session_reuse",
        "an_import_that_registers_nothing_leaves_the_pool_intact",
        ((1, 2),),
    ),
    ("session_reuse", "capacity_one_is_unaffected_by_staleness_eviction", ((1, 2),)),
)
ORDINARY_STALENESS_IMPORT_TESTS = (
    (
        "session_reuse",
        "a_staleness_eviction_does_not_disable_the_reuse_hint",
        ((1, 4),),
    ),
)
ORDINARY_STALENESS_CASES = frozenset(
    name
    for _, name, _ in (
        *ORDINARY_STALENESS_BATCH_TESTS,
        *ORDINARY_STALENESS_IMPORT_TESTS,
    )
)
ORDINARY_VERIFICATION_TESTS = (
    (
        "typed_session",
        "verify_declaration_not_found_carries_similar_declarations",
        ((1, 1),),
    ),
    (
        "typed_session",
        "verify_declaration_accepts_closed_theorem_and_rejects_sorry",
        ((1, 2),),
    ),
    (
        "typed_session",
        "module_public_section_resolves_to_unique_declaration",
        ((1, 2),),
    ),
    (
        "typed_session",
        "verify_declaration_reports_ambiguous_with_candidates",
        ((1, 1),),
    ),
    (
        "typed_session",
        "verify_declaration_ambiguous_open_reference_does_not_restart_child",
        ((1, 1),),
    ),
    (
        "typed_session",
        "verify_declaration_reports_needs_build_for_unbuilt_import",
        ((1, 1),),
    ),
)
ORDINARY_VERIFICATION_BATCH_TESTS = (
    (
        "typed_session",
        "declaration_verification_batch_returns_ordered_rows_through_worker",
        ((1, 1),),
    ),
)
JSON_GRANT = b"authorize-consumer-load"
META_TERM_GRANT = b"authorize-fixed-meta-term"
MODULE_QUERY_GRANT = b"authorize-fixed-module-query"
PROOF_GRANT = b"authorize-fixed-proof-attempt"
BATCH_GRANT = b"authorize-fixed-module-batch"
VERIFICATION_GRANT = b"authorize-fixed-declaration-verification"
VERIFICATION_BATCH_GRANT = b"authorize-fixed-verification-batch"
IMPORT_GRANT = b"authorize-checked-typed-import"
ORDINARY_WIDE_REUSE_CASES = frozenset(
    {
        "switching_back_to_a_pooled_import_set_does_not_reimport",
        "capacity_one_restores_the_pre_pool_child",
        "a_mispredicted_reuse_restarts_before_the_actual_import",
        "a_reused_session_carries_no_state_from_the_previous_one",
    }
)
SDK_SHAPE_GRANTS_BY_WORKER = {
    "metadata_doctor_and_version_use_generic_capability_shapes": (1,),
    "streaming_commands_use_builder_and_typed_facade": (1, 1, 1, 1),
    "timeout_cancellation_and_fatal_exit_have_distinct_worker_outcomes": (
        1,
        0,
        1,
        0,
        1,
    ),
    "worker_cycle_leaves_shape_fixture_usable": (1, 1),
}
PLANNER_SHAPE_TESTS = {
    "readiness_commands_run_through_planner_pool_lease_and_typed_facade": (
        "lean_dup_readiness",
        (1,),
    ),
    "readiness_operational_failures_and_recovery_are_typed": (
        "lean_dup_readiness",
        (1, 1, 1, 1, 1),
    ),
    "mathlib_scale_fixture_runs_through_planner_pool_lease_and_typed_command": (
        "mathlib_scale_fixture",
        (1,),
    ),
    "mathlib_scale_fixture_reports_timeout_cancellation_and_fatal_exit_distinctly": (
        "mathlib_scale_fixture",
        (1, 0, 1, 0, 1),
    ),
}
IMPORT_PLANNING_CASE = "planned_batches_execute_through_pool_with_one_import_group"
ORDINARY_GRANTS_BY_WORKER = {
    **{name: grants for name, (_, grants) in PLANNER_SHAPE_TESTS.items()},
    IMPORT_PLANNING_CASE: (1, 1, 1),
    **SDK_SHAPE_GRANTS_BY_WORKER,
    "child_fatal_exit_invalidates_lease_and_next_acquire_replaces_worker": (1, 1),
    "cancellation_invalidates_current_lease_but_future_lease_can_continue": (1, 1),
    "timeout_invalidates_current_lease_but_future_lease_can_continue": (1, 1),
    "request_timeout_cycles_child_and_invalidates_session": (1, 0),
    "cancellation_during_backpressure_invalidates_lease": (1, 0),
    "typed_streaming_command_row_decode_error_names_stream_and_sequence": (1, 0),
    "row_sink_cancellation_cycles_child_and_invalidates_session": (1, 0),
    "a_mispredicted_reuse_restarts_before_the_actual_import": (3, 2),
    "a_capacity_eviction_clears_the_snapshot_cache": (2, 2),
}

POOL_BATCH_GRANTS = {
    "lease_module_query_batch_reuses_one_warm_session": 3,
    "lease_module_query_batch_preserves_item_level_budget_failures": 1,
    "lease_module_query_batch_pre_cancelled_token_sends_no_request": 0,
}
ORDINARY_MIXED_GRANTS = {
    "attempt_proof_bad_candidate_returns_diagnostics_and_session_survives": (
        PROOF_GRANT,
        MODULE_QUERY_GRANT,
    ),
    "process_module_query_batch_obeys_total_budget_without_killing_worker": (
        BATCH_GRANT,
        META_TERM_GRANT,
    ),
}
ORDINARY_AUDITED_TESTS = (
    *(
        (target, name, counts, event)
        for cases, event in (
            (ORDINARY_JSON_TESTS, JSON_GRANT),
            (ORDINARY_META_TESTS, META_TERM_GRANT),
            (ORDINARY_ADAPTER_TESTS, ADAPTER_IMPORT_GRANT),
            (ORDINARY_ADAPTER_KERNEL_TESTS, ORDINARY_ADAPTER_KERNEL_GRANT),
            (ORDINARY_MODULE_QUERY_TESTS, MODULE_QUERY_GRANT),
            (ORDINARY_PROOF_TESTS, PROOF_GRANT),
            (ORDINARY_BATCH_TESTS, BATCH_GRANT),
            (ORDINARY_REUSE_BATCH_TESTS, BATCH_GRANT),
            (ORDINARY_REUSE_IMPORT_TESTS, IMPORT_GRANT),
            (ORDINARY_STALENESS_BATCH_TESTS, BATCH_GRANT),
            (ORDINARY_STALENESS_IMPORT_TESTS, IMPORT_GRANT),
            (ORDINARY_VERIFICATION_TESTS, VERIFICATION_GRANT),
            (ORDINARY_VERIFICATION_BATCH_TESTS, VERIFICATION_BATCH_GRANT),
        )
        for target, name, counts in cases
    ),
    *(
        ("typed_session", name, ((1, len(events)),), events[0])
        for name, events in ORDINARY_MIXED_GRANTS.items()
    ),
)
ORDINARY_GRANT_EVENTS = frozenset(
    event for *_, event in ORDINARY_AUDITED_TESTS if event != IMPORT_GRANT
)
MAX_FIXTURE_WORKER_BYTES = 256 * 1024**2
GENERATED_OUTPUT_KEY = "lean-4.36.0"
DARWIN_ARM64_PINS = {
    "lean": "a4c5d3dfc1672faf014e59bd17724035259ff0d22a4ffbdf5a75e142f564817e",
    "lake": LAKE_SHA256,
    "runtime": "9fa4e429820684b59ff37740ca77744a2cd6de70e6e963ef0904760c6921aed5",
    "runtime_name": "libleanshared.dylib",
    "nextest_runner_variable": "CARGO_TARGET_AARCH64_APPLE_DARWIN_RUNNER",
    "nextest_runner": "/usr/bin/env -u DYLD_FALLBACK_LIBRARY_PATH -u DYLD_LIBRARY_PATH",
}
LINUX_X86_64_NEXTEST_RUNNER = {
    "nextest_runner_variable": "CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUNNER",
    "nextest_runner": "/usr/bin/env -u LD_LIBRARY_PATH",
}

LINUX_X86_64_BROKEN_IMPORT_PINS = {
    "lean": "db7f559d05795e7af52e42bc966c90240a39dcb9712161c54c97afa546e20c00",
    "lake": "42678ccbc2605e2f892540dc7236e009fdd1224e823d104dfd72725d2f1a52d2",
    "runtime": "e2c403b411497bbc325d0a26bf57ba0132271750bf6d84e32cd61a9db4777252",
    "runtime_name": "libleanshared.so",
    **LINUX_X86_64_NEXTEST_RUNNER,
}


def require(condition, message):
    if not condition:
        raise ValueError(message)


@contextmanager
def ordinary_state_lock():
    directory = STATE_LOCK.parent
    directory.mkdir(parents=True, exist_ok=True)
    require(
        directory == directory.resolve(strict=True), "ordinary state parent is aliased"
    )
    fd = os.open(STATE_LOCK, os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW, 0o600)
    lock = os.fdopen(fd, "r+b")
    try:
        stat = os.fstat(lock.fileno())
        require(
            stat.st_uid == os.getuid() and stat.st_mode & 0o077 == 0,
            "ordinary state lock is not private to its owner",
        )
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError as error:
            raise ValueError(
                "another ordinary metadata action owns the stable state lock"
            ) from error
        yield
    finally:
        lock.close()


def write_phase(state, phase):
    pending = state / ".phase.new"
    with pending.open("x") as stream:
        stream.write(phase + "\n")
        stream.flush()
        os.fsync(stream.fileno())
    pending.replace(state / "phase")
    directory = os.open(state, os.O_RDONLY | os.O_DIRECTORY)
    try:
        os.fsync(directory)
    finally:
        os.close(directory)


def digest(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def verified_platform(system=None, machine=None, *, broken_import=False):
    system = platform.system() if system is None else system
    machine = platform.machine() if machine is None else machine
    if (system, machine) == ("Darwin", "arm64"):
        return DARWIN_ARM64_PINS
    if (system, machine) == ("Linux", "x86_64") and broken_import:
        return LINUX_X86_64_BROKEN_IMPORT_PINS
    if system == "Linux":
        raise ValueError(
            "Linux ordinary dispatch awaits verified Lean release, helper and worker artifact pins"
        )
    raise ValueError(f"unsupported metadata qualification platform: {system} {machine}")


def refuse_inherited_loader_overrides(system=None, environ=None):
    system = platform.system() if system is None else system
    environ = os.environ if environ is None else environ
    if system == "Linux":
        overrides = sorted(
            name
            for name in environ
            if name == "GLIBC_TUNABLES"
            or name.startswith(("DYLD_", "__XPC_DYLD_", "LD_"))
        )
        require(
            not overrides, f"inherited Linux loader override: {', '.join(overrides)}"
        )


def select_nextest_runner(env, profile):
    """Select the reviewed target runner without changing the runner-entry environment."""
    variable = profile["nextest_runner_variable"]
    command = profile["nextest_runner"]
    require(variable not in env, f"duplicate target runner entry: {variable}")
    env[variable] = command


def installed_lean_release(profile=None):
    profile = verified_platform() if profile is None else profile
    pin = (SDK / "lean-toolchain").read_text().strip()
    require(pin and "\n" not in pin, "expected one repository Lean toolchain pin")
    version = pin.removeprefix("leanprover/lean4:v")
    require(version and version != pin, "expected a named Lean release")
    # Check the selected binaries before asking the Lean executable for its prefix.
    binary = Path(
        subprocess.check_output(["elan", "run", pin, "elan", "which", "lean"], text=True).rstrip("\n")
    )
    release = binary.parent.parent.resolve(strict=True)
    require(binary == release / "bin/lean", "unexpected Elan-selected Lean binary")
    elan_home = Path(os.environ.get("ELAN_HOME", Path.home() / ".elan")).expanduser()
    toolchains = elan_home / "toolchains"
    require(
        release.parent == toolchains.resolve(strict=True),
        "repository Lean toolchain did not resolve inside the installed Elan toolchains directory",
    )
    inherited = os.environ.get("LEAN_SYSROOT")
    if inherited is not None:
        require(
            Path(inherited).expanduser().resolve(strict=True) == release,
            "inherited LEAN_SYSROOT differs from the repository Lean toolchain pin",
        )
    check_file(release / "bin/lean", profile["lean"])
    check_file(release / "bin/lake", profile["lake"])
    check_file(release / "lib/lean" / profile["runtime_name"], profile["runtime"])
    prefix = subprocess.check_output(
        [SDK / "scripts/lean-release-prefix.sh", version], text=True
    ).rstrip("\n")
    require(Path(prefix) == release, "reported Lean prefix differs from the verified release")
    return release


def check_tracked_fixture_sources():
    for path in [
        APP / "Cargo.toml",
        APP / "Cargo.lock",
        APP / "build.rs",
        APP / "seal.py",
        APP / "src/lib.rs",
        APP / "src/bin/control.rs",
        APP / "src/bin/prepare.rs",
        APP / "src/bin/metadata-worker.rs",
        SDK / "fixtures/metadata-qualification/lean/lakefile.lean",
        SDK / "fixtures/metadata-qualification/lean/lake-manifest.json",
        SDK / "fixtures/metadata-qualification/lean/lean-toolchain",
        SDK / "fixtures/metadata-qualification/lean/LeanRsInteropConsumer.lean",
        SDK
        / "fixtures/metadata-qualification/lean/LeanRsInteropConsumer/Callback.lean",
    ]:
        require(
            path == path.resolve(strict=True) and path.is_file(),
            f"expected canonical fixture source: {path}",
        )
        relative = path.relative_to(SDK)
        subprocess.run(
            [
                "/usr/bin/git",
                "--no-replace-objects",
                "-C",
                SDK,
                "ls-files",
                "--error-unmatch",
                "--",
                relative,
            ],
            check=True,
            stdout=subprocess.DEVNULL,
        )


def check_file(path, approved):
    require(
        path == path.resolve(strict=True) and path.is_file(),
        f"expected a canonical file: {path}",
    )
    require(digest(path) == approved, f"SHA-256 mismatch: {path}")


def installed_library_fingerprint(release):
    """Hash actual bytes and exact layout, without saving a cache or report."""
    root = release / "lib/lean"
    require(root.resolve(strict=True) == root, "aliased installed Lean library root")
    fingerprint = hashlib.sha256()

    def identity(metadata):
        return (
            metadata.st_mode,
            metadata.st_dev,
            metadata.st_ino,
            metadata.st_size,
            metadata.st_mtime_ns,
            metadata.st_ctime_ns,
        )

    def visit(path):
        before = path.lstat()
        relative = os.fsencode(path.relative_to(root))
        fingerprint.update(len(relative).to_bytes(8, "big"))
        fingerprint.update(relative)
        fingerprint.update(repr(identity(before)).encode("ascii") + b"\0")
        if stat.S_ISDIR(before.st_mode):
            for child in sorted(
                path.iterdir(), key=lambda entry: os.fsencode(entry.name)
            ):
                visit(child)
        else:
            require(
                stat.S_ISREG(before.st_mode),
                f"non-regular entry in installed Lean library tree: {path}",
            )
            with open(path, "rb", opener=open_regular_no_follow) as stream:
                require(
                    identity(os.fstat(stream.fileno())) == identity(before),
                    f"installed Lean library entry changed before read: {path}",
                )
                fingerprint.update(hashlib.file_digest(stream, "sha256").digest())
        require(
            identity(path.lstat()) == identity(before),
            f"installed Lean library entry changed during read: {path}",
        )

    visit(root)
    return fingerprint.digest()


def open_regular_no_follow(path, flags):
    return os.open(path, flags | os.O_NOFOLLOW | os.O_NONBLOCK)


def run_ordinary_with_release_bookends(release, run, *, emit_host_selection=False):
    # Only this launcher opts in. The selected Elan release must be unchanged
    # throughout the run; project readers do not lock Elan.
    before = installed_library_fingerprint(release)
    if emit_host_selection:
        assignments = io.StringIO()
        with redirect_stdout(assignments):
            run()
    else:
        run()
    # If the first error stopped the suite before closure, do not obscure it
    # with another check or publish a selection.
    require(
        installed_library_fingerprint(release) == before,
        "installed Lean library tree changed during ordinary qualification",
    )
    if emit_host_selection:
        print(assignments.getvalue(), end="")


def fixture_sources(tree):
    git = ["/usr/bin/git", "--no-replace-objects", "-C", SDK]
    entries = subprocess.check_output([*git, "ls-tree", "-rz", tree])
    sources = {}
    for entry in entries.rstrip(b"\0").split(b"\0"):
        header, name = entry.split(b"\t", 1)
        mode, kind, oid = header.split()
        require(
            (mode, kind) == (b"100644", b"blob"),
            "expected regular fixture source files",
        )
        sources[Path(os.fsdecode(name))] = subprocess.check_output(
            [*git, "cat-file", "blob", oid]
        )
    require(Path("Cargo.toml") in sources, "missing fixture app in pinned Git tree")
    return sources


def reviewed_writer(path, approved):
    require(
        path == path.resolve(strict=True)
        and path.is_file()
        and os.access(path, os.X_OK),
        f"expected a canonical executable writer: {path}",
    )
    data = path.read_bytes()
    require(hashlib.sha256(data).hexdigest() == approved, f"SHA-256 mismatch: {path}")
    return data


def space(path):
    require(
        shutil.disk_usage(path).free >= 25 * 2**30, f"less than 25 GiB free on {path}"
    )


def build_environment(release, scratch):
    # These are trusted build tools. In particular, no loader overrides or Cargo wrappers
    # cross into the readiness executable, writer, guardian or child.
    prefix = release
    cargo = shutil.which("cargo")
    require(cargo is not None, "could not locate cargo for metadata qualification")
    rust_bins = [Path(cargo).absolute().parent]
    # Hosted runners can install this Cargo subcommand separately from rustup's proxies.
    if nextest := shutil.which("cargo-nextest"):
        rust_bins.append(Path(nextest).absolute().parent)
    path = [prefix / "bin", *rust_bins, Path(sys.executable).parent, "/usr/bin", "/bin"]
    return {
        "HOME": str(Path.home()),
        "PATH": os.pathsep.join(dict.fromkeys(map(str, path))),
        **{name: os.environ[name] for name in ("CARGO_HOME", "RUSTUP_HOME") if name in os.environ},
        "LEAN_SYSROOT": str(prefix),
        "LEAN_NUM_THREADS": "1",
        "CARGO_BUILD_JOBS": "1",
        "CARGO_INCREMENTAL": "0",
        "CARGO_TARGET_DIR": str(TARGET),
        "CARGO_NET_OFFLINE": "true",
        "TMPDIR": str(scratch),
    }


def environment(state, helper, release, deployment=None):
    env = build_environment(release, TARGET / "tmp")
    env.update(
        LEAN_HOST_MCP_WRITER_EXECUTABLE=str(helper),
        D3_HELPER=str(helper),
        LEAN_RS_WORKER_OWNERS_DIRECTORY=str(state / "owners"),
    )
    if deployment is not None:
        env.update(
            LEAN_RS_METADATA_DEPLOYMENT_RECORD=str(deployment),
            LEAN_RS_METADATA_DEPLOYMENT_SHA256=digest(deployment),
            LEAN_RS_METADATA_AUDIT_PATH=str(state / "app/native-events.log"),
        )
    return env


def provision(
    writer,
    writer_sha256,
    release,
    typed_session=False,
    disposable_clean=False,
    verification=False,
):
    require(
        not (typed_session or disposable_clean or verification),
        "typed, verification and disposable-clean provisioning is unavailable in the canonical ordinary runner",
    )
    space(TARGET)
    check_tracked_fixture_sources()
    require(not STATE.exists(), f"ordinary deployment state already exists: {STATE}")
    require(
        not SOCKET_ROOT.exists(), f"ordinary socket root already exists: {SOCKET_ROOT}"
    )
    STATE.mkdir(parents=True, mode=0o700)
    SOCKET_ROOT.mkdir(mode=0o700)
    (STATE / "owners").mkdir(mode=0o700)
    (STATE / "app").mkdir(mode=0o700)
    helper = writer.resolve(strict=True)
    require(
        digest(helper) == writer_sha256, "reviewed helper changed before provisioning"
    )
    env = environment(STATE, helper, release)
    cargo = ["cargo", "build", "--locked", "--offline", "-j1"]
    for cmd in (
        [*cargo, "--bin", "prepare"],
        [str(TARGET / "debug/prepare"), str(STATE)],
    ):
        space(TARGET)
        subprocess.run(cmd, cwd=APP, env=env, check=True)
    record = STATE / "deployment.json"
    env = environment(STATE, helper, release, record)
    space(TARGET)
    subprocess.run(
        [
            *cargo,
            "--features",
            "admitted",
            "--bin",
            "metadata-worker",
            "--bin",
            "control",
        ],
        cwd=APP,
        env=env,
        check=True,
    )
    space(TARGET)
    write_phase(STATE, "prepared")
    for path in (TARGET / "debug/metadata-worker", TARGET / "debug/control", record):
        print(f"{digest(path)}  {path}")


def check_clean_deployment(root, approved_deployment):
    require(
        root.name.startswith("lean-e0-clean-"),
        "requires a fresh disposable clean deployment",
    )
    permit = root / "clean-ready"
    require(
        permit == permit.resolve(strict=True)
        and permit.read_text() == approved_deployment,
        "clean permit differs from the reviewed deployment",
    )
    require(
        not (root / "clean-started").exists()
        and not (root / "app/native-events.log").exists()
        and all(
            entry.name == ".admission.lock" for entry in (root / "owners").iterdir()
        ),
        "clean deployment has already been used",
    )
    project = root / "app/lean"
    build = project / ".lake/build"
    require(
        project == project.resolve(strict=True)
        and not project.is_relative_to(SDK)
        and build == build.resolve(strict=True)
        and build.is_dir(),
        "expected private, canonical Lake build directory",
    )
    # The selector rechecks these inputs under its writer before re-elaborating the package.
    config = {}
    for name in ("lakefile.lean", "lake-manifest.json", "lean-toolchain"):
        original = subprocess.check_output(
            [
                "/usr/bin/git",
                "--no-replace-objects",
                "-C",
                SDK,
                "show",
                f"{TYPED_SDK}:fixtures/lean/{name}",
            ]
        )
        config[name] = hashlib.sha256(original).hexdigest()
        check_file(project / name, config[name])
    check_file(root / "release/bin/lake", LAKE_SHA256)
    return {"deployment": approved_deployment, "config": config, "lake": LAKE_SHA256}


def scalar_fixture_inputs(project, deployment, helper, helper_sha256):
    image = project / ".lake/build/lib/liblean__rs__fixture_LeanRsFixture.dylib"
    require(
        str(project) == deployment["project"]
        and str(image) == deployment["consumer_image"]
        and str(image) == json.loads(deployment["consumer_json"])["primary_dylib"],
        "scalar test requires the sealed typed consumer image",
    )
    device, inode, sha256 = deployment["files"][str(image)]
    require(
        image == image.resolve(strict=True) and image.is_file(),
        "expected canonical scalar fixture image",
    )
    with image.open("rb") as source:
        stat = os.fstat(source.fileno())
        require(
            (stat.st_dev, stat.st_ino) == (device, inode),
            "scalar fixture image identity changed",
        )
        require(
            hashlib.file_digest(source, "sha256").hexdigest() == sha256,
            "scalar fixture image digest changed",
        )
    return {
        "LEAN_RS_TEST_SCALAR_FIXTURE": json.dumps([str(project), device, inode]),
        "LEAN_RS_PROFILE_FIXTURE_SHA256": sha256,
        "LEAN_RS_PROFILE_READER": str(helper),
        "LEAN_RS_PROFILE_READER_SHA256": helper_sha256,
    }


def check_installed_toolchain_pins(deployment, release):
    selected = [
        (Path(name), pin)
        for name, pin in deployment["files"].items()
        if Path(name).is_relative_to(release)
    ]
    require(selected, "deployment does not pin the selected installed Lean toolchain")
    for path, (device, inode, sha256) in selected:
        stat = path.stat()
        require(
            (stat.st_dev, stat.st_ino) == (device, inode),
            f"installed Lean toolchain file identity changed: {path}",
        )
        check_file(path, sha256)


def check_unavailable_typed_audit(events, before, verification):
    """Retained policy assertions for typed modes while this runner cannot provision them safely."""
    after = events.read_bytes()
    require(after.startswith(before), "native fixture event history changed")
    grants = [line.split(" ", 1) for line in after[len(before) :].decode().splitlines()]
    require(
        grants and len({pid for pid, _ in grants}) == 1,
        "expected a single fresh fixture worker",
    )
    names = [event for _, event in grants]
    required = [
        "verified-deployment",
        "authorize-host-load",
        "verified-global-interop-initializer",
        "authorize-LeanRsFixture-load",
        "authorize-Handles-SourceRanges-import",
    ]
    require(
        names[: len(required)] == required,
        f"unexpected fixture admission order: {names}",
    )
    if verification:
        expected = required + [
            "authorize-verify-proved",
            "authorize-verify-sorry",
            "authorize-verify-unavailable",
            "refused-verification-options",
            "authorize-verify-truncated",
        ]
        require(names == expected, f"unexpected verification grants: {names}")


def qualify(
    root,
    writer,
    writer_sha256,
    approved_worker,
    approved_deployment,
    typed_session=False,
    disposable_clean=False,
    verification=False,
    unknown_declaration=False,
    pool_validator_recovery=False,
    release=None,
    approved_control=None,
    profile=None,
):
    require(
        not (typed_session or disposable_clean or verification or unknown_declaration),
        "typed, verification and disposable-clean selectors are unavailable in the canonical ordinary runner",
    )
    profile = verified_platform() if profile is None else profile
    release = installed_lean_release(profile) if release is None else release
    space(TARGET)
    require(
        not pool_validator_recovery
        or (
            not typed_session
            and not disposable_clean
            and not verification
            and not unknown_declaration
        ),
        "pool validator recovery requires its named ordinary metadata route",
    )
    require(
        root == STATE and root == root.resolve(strict=True),
        "unexpected or aliased ordinary deployment state path",
    )
    require(
        root.stat().st_uid == os.getuid() and root.stat().st_mode & 0o777 == 0o700,
        "ordinary deployment state is not private to its owner",
    )
    require(
        SOCKET_ROOT == SOCKET_ROOT.resolve(strict=True)
        and SOCKET_ROOT.is_dir()
        and SOCKET_ROOT.stat().st_uid == os.geteuid()
        and SOCKET_ROOT.stat().st_mode & 0o777 == 0o700,
        "ordinary socket root is not canonical and private",
    )
    require(
        not any(SOCKET_ROOT.iterdir()),
        "ordinary socket root is not empty before dispatch",
    )
    check_tracked_fixture_sources()
    worker = TARGET / "debug/metadata-worker"
    control = TARGET / "debug/control"
    record = root / "deployment.json"
    check_file(worker, approved_worker)
    check_file(control, approved_control)
    check_file(record, approved_deployment)
    d = json.loads(record.read_bytes())
    project = SDK / "fixtures/metadata-qualification/lean"
    require(
        d.get("typed_session") is False and d.get("verification") is False,
        "ordinary deployment does not declare the ordinary fixture policy",
    )
    require(
        d.get("generated_output_key") == GENERATED_OUTPUT_KEY,
        "ordinary deployment selected a different generated-output slot",
    )
    require(
        d["sysroot"] == str(release) and d["project"] == str(project),
        "deployment record belongs to a different application or release",
    )
    # Elan does not participate in the project locks. Recheck the selected installed
    # toolchain before and after the smoke, and report any concurrent change as failure.
    check_installed_toolchain_pins(d, release)
    libdir = release / "lib/lean"
    suffix = Path(profile["runtime_name"]).suffix
    sealed = {
        Path(name): pin
        for name, pin in d["files"].items()
        if Path(name).parent == libdir and name.endswith(suffix)
    }
    require(
        libdir / profile["runtime_name"] in sealed
        and set(libdir.glob(f"*{suffix}")) == sealed.keys(),
        "selected runtime library inventory changed",
    )
    for image, (device, inode, sha256) in sealed.items():
        stat = image.stat()
        require(
            (stat.st_dev, stat.st_ino) == (device, inode),
            f"runtime file identity changed: {image}",
        )
        check_file(image, sha256)
    manifest = Path(d["consumer_manifest"])
    require(
        manifest.is_relative_to(project)
        and manifest == manifest.resolve(strict=True)
        and manifest.read_bytes() == d["consumer_json"].encode(),
        "consumer manifest differs from review",
    )
    owners = root / "owners"
    require(
        owners == owners.resolve(strict=True) and owners.is_dir(),
        "missing deployment ownership registry",
    )
    helper = writer.resolve(strict=True)
    require(
        digest(helper) == writer_sha256, "reviewed helper changed before qualification"
    )
    env = environment(root, helper, release, record)
    require(
        (root / "phase").read_text() == "prepared\n",
        "ordinary deployment was already dispatched",
    )
    write_phase(root, "dispatched")
    # Only the independently approved worker can be executed, even for a read-only probe.
    ready = subprocess.check_output(
        [
            worker,
            "--lean-rs-ready-host-artifacts-generated",
            SOCKET_ROOT,
            GENERATED_OUTPUT_KEY,
        ],
        cwd=APP,
        env=env,
    )
    expected = {
        "protocol": 2,
        "shim_manifest_path": d["shim_manifest"],
        "shim_manifest_json": d["shim_json"],
        "artifact_roots": d["shim_roots"],
        "shim_search_paths": d["shim_paths"],
    }
    require(
        json.loads(ready) == expected,
        "readiness descriptor differs from the reviewed deployment",
    )
    variable, target, tests = (
        "LEAN_RS_TEST_QUALIFIED_METADATA",
        "capability_builder",
        TESTS,
    )
    if pool_validator_recovery:
        tests = (
            "qualified_pool::failed_validator_releases_pool_slot_before_fresh_key",
        )
    selection = {"manifest": str(manifest)}
    env[variable] = json.dumps(
        dict(
            worker=str(worker),
            worker_sha256=approved_worker,
            helper=str(helper),
            helper_sha256=writer_sha256,
            owners=str(owners),
            sysroot=d["sysroot"],
            prepared=expected,
            **selection,
        )
    )
    select_nextest_runner(env, profile)
    space(TARGET)
    events = root / "app/native-events.log"
    before = events.read_bytes() if events.exists() else b""
    for name in tests:
        subprocess.run(
            [
                "cargo",
                "nextest",
                "run",
                "--locked",
                "--offline",
                "-j1",
                "--no-tests",
                "fail",
                "-p",
                "lean-rs-worker-child",
                "--test",
                target,
                *(
                    [
                        "--run-ignored",
                        "only",
                        "--retries",
                        "0",
                        "--success-output",
                        "immediate",
                    ]
                    if pool_validator_recovery
                    else []
                ),
                "-E",
                f"test(={name})",
            ],
            cwd=SDK,
            env=env,
            check=True,
        )
    after = events.read_bytes()
    require(after.startswith(before), "native fixture event history changed")
    grants = [line.split(" ", 1) for line in after[len(before) :].decode().splitlines()]
    blocks = []
    for pid, event in grants:
        if not blocks or blocks[-1][0] != pid:
            blocks.append((pid, []))
        blocks[-1][1].append(event)
    require(
        len(blocks) == 2 and blocks[0][0] != blocks[1][0],
        "expected exactly two distinct authorized fixture workers",
    )
    prefix = [
        "verified-deployment",
        "authorize-consumer-load",
        "authorize-host-load",
        "verified-global-interop-initializer",
    ]
    for _, names in blocks:
        require(
            names[: len(prefix)] == prefix
            and len(names) > len(prefix)
            and all(
                event == "authorize-Callback-import" for event in names[len(prefix) :]
            ),
            f"unexpected ordinary fixture admission order: {names}",
        )
    subprocess.run([control, "collect-closed"], cwd=APP, env=env, check=True)
    owner_entries = list(owners.iterdir())
    require(
        len(owner_entries) == 1
        and owner_entries[0].name == ".admission.lock"
        and owner_entries[0].is_file()
        and owner_entries[0] == owner_entries[0].resolve(strict=True)
        and owner_entries[0].stat().st_uid == os.getuid(),
        "worker ownership records remain after the selected checks",
    )
    check_installed_toolchain_pins(d, release)
    space(TARGET)
    require(
        not any(SOCKET_ROOT.iterdir()), "ordinary socket root retains a launch entry"
    )
    SOCKET_ROOT.rmdir()
    shutil.rmtree(root)


def fixture_worker_digest(worker):
    require(
        worker == worker.resolve(strict=True)
        and worker.is_file()
        and os.access(worker, os.X_OK)
        and worker.stat().st_size <= MAX_FIXTURE_WORKER_BYTES,
        "selected fixture worker is not a bounded executable",
    )
    with worker.open("rb") as stream:
        expected = os.fstat(stream.fileno()).st_size
        require(
            expected <= MAX_FIXTURE_WORKER_BYTES,
            "selected fixture worker grew before hashing",
        )
        sha256 = hashlib.sha256()
        remaining = expected
        while chunk := stream.read(min(64 * 1024, remaining + 1)):
            remaining -= len(chunk)
            require(remaining >= 0, "selected fixture worker grew while hashing")
            sha256.update(chunk)
        require(remaining == 0, "selected fixture worker shrank while hashing")
    return sha256.hexdigest()


def remove_owned_state(state, owner):
    closed = state.stat()
    require(
        state == state.resolve(strict=True)
        and (closed.st_dev, closed.st_ino) == (owner.st_dev, owner.st_ino),
        f"fixture directory changed: {state}",
    )
    shutil.rmtree(state)


def scoped_tests(binary, names):
    cases = " | ".join(f"test(={name})" for name in names)
    return f"(binary(={binary}) & ({cases}))"


def ordinary_app_tests():
    typed = scoped_tests("typed_session", ORDINARY_TYPED_TESTS)
    reuse = scoped_tests("session_reuse", ORDINARY_SESSION_REUSE_TESTS)
    return f"{typed} | {reuse}"


def typed_audit(events):
    with events.open("rb") as stream:
        data = stream.read(16 * 1024 + 1)
    require(
        0 < len(data) <= 16 * 1024 and data.endswith(b"\n"),
        "missing or incomplete typed worker audit",
    )
    return data


def check_typed_children_closed(
    data, *, case=None, expected_counts=(), grant_event=JSON_GRANT, grant_order=None
):
    started = []
    admitted = set()
    workers = set()
    granted = []
    grant_pids = []
    for line in data.splitlines():
        raw, event = line.split(b" ", 1)
        require(raw.isdigit() and int(raw) > 0, "invalid typed worker PID")
        pid = int(raw)
        workers.add(pid)
        if event == b"worker-started":
            started.append(pid)
        if event == b"verified-deployment":
            admitted.add(pid)
        if (
            event in ORDINARY_GRANT_EVENTS
            or event == grant_event
            or event.startswith(
                (
                    b"authorize-fixed-stream-",
                    b"authorize-fixed-metadata-",
                    b"authorize-fixed-doctor-",
                    b"authorize-fixed-signature-probe-",
                    b"authorize-fixed-json-",
                    b"authorize-fixed-shape-",
                    b"admit-fixed-no-exports-",
                    b"admit-fixed-direct-interop-",
                )
            )
        ):
            granted.append(event)
            grant_pids.append(pid)
    if case is None:
        require(
            len(admitted) >= ORDINARY_APP_TEST_COUNT,
            "typed tests did not start the expected admitted workers",
        )
    else:
        require(
            (len(started), len(granted)) in expected_counts
            and len(started) == len(workers)
            and set(started) == workers
            and admitted == set(grant_pids) <= workers
            and (grant_event != JSON_GRANT or len(granted) == len(admitted)),
            f"{case}: wrong started worker / {grant_event.decode()} counts",
        )
        expected = grant_order or ORDINARY_MIXED_GRANTS.get(
            case, (grant_event,) * len(granted)
        )
        require(
            tuple(granted) == expected,
            f"{case}: wrong started worker / {grant_event.decode()} counts or grant order",
        )
        by_worker = ORDINARY_GRANTS_BY_WORKER.get(case)
        if by_worker is not None:
            require(
                tuple(grant_pids.count(pid) for pid in started) == by_worker,
                f"{case}: wrong grant counts per worker",
            )
    for pid in workers:
        try:
            os.kill(pid, 0)
        except ProcessLookupError:
            continue
        raise ValueError(f"typed worker PID {pid} remains; keeping fixture state")


STREAM_TESTS = (
    ("streaming_runner", "successful_stream_delivers_rows_with_per_stream_sequences"),
    ("typed_command", "typed_streaming_command_decodes_rows_and_summary"),
    (
        "typed_command",
        "typed_streaming_command_row_decode_error_names_stream_and_sequence",
    ),
    ("typed_command", "typed_streaming_command_summary_decode_error_is_typed"),
    ("typed_command", "raw_row_streaming_remains_available"),
    ("streaming_runner", "row_sink_cancellation_cycles_child_and_invalidates_session"),
    ("streaming_runner", "diagnostic_sink_panic_is_typed"),
    ("streaming_runner", "malformed_row_json_is_typed"),
    ("streaming_runner", "missing_stream_or_payload_is_typed"),
    ("streaming_runner", "nonzero_export_status_is_typed"),
    ("streaming_runner", "callback_status_error_is_typed"),
    ("streaming_runner", "large_stream_records_live_forwarding_throughput_and_rss"),
    (
        "typed_command",
        "helper_chunked_stream_delivers_rows_diagnostics_progress_and_metadata",
    ),
    ("typed_command", "helper_chunked_stream_reports_bounded_chunk_errors"),
    ("typed_command", "helper_completion_order_stream_uses_same_typed_surface"),
    (
        "typed_command",
        "typed_streaming_command_decodes_large_payload_from_performance_path",
    ),
    ("streaming_runner", "row_before_child_panic_is_delivered_before_terminal_failure"),
    ("streaming_runner", "request_timeout_cycles_child_and_invalidates_session"),
    ("observability_backpressure", "pool_and_lease_snapshots_record_stream_throughput"),
    (
        "observability_backpressure",
        "slow_sink_applies_bounded_backpressure_without_losing_rows",
    ),
    (
        "observability_backpressure",
        "cancellation_during_backpressure_invalidates_lease",
    ),
    (
        "observability_backpressure",
        "fatal_exit_after_buffered_rows_records_failure_without_commit",
    ),
    ("pool", "child_fatal_exit_invalidates_lease_and_next_acquire_replaces_worker"),
    ("pool", "cancellation_invalidates_current_lease_but_future_lease_can_continue"),
    ("pool", "timeout_invalidates_current_lease_but_future_lease_can_continue"),
    ("typed_command", "typed_json_command_decode_error_is_typed"),
)


INTEROP_METADATA_TESTS = (
    ("capability_builder", "builder_opens_worker_capability_and_validates_metadata"),
    ("capability_builder", "metadata_mismatch_is_reported_by_bootstrap_check"),
    ("capability_builder", "metadata_validation_failure_is_typed"),
    ("pool", "metadata_mismatch_is_typed"),
)
INTEROP_METADATA_CASES = frozenset(name for _, name in INTEROP_METADATA_TESTS)
NO_EXPORTS_CASE = "missing_metadata_and_doctor_exports_are_typed_worker_errors"
INTEROP_DOCTOR_EVENTS = {
    "runtime_and_capability_metadata_are_separate": "authorize-fixed-metadata",
    "doctor_reports_pass_warning_and_error_diagnostics": "authorize-fixed-doctor",
    "malformed_metadata_and_doctor_json_are_typed": "authorize-fixed-doctor",
    NO_EXPORTS_CASE: "admit-fixed-no-exports",
}
INTEROP_SIGNATURE_CASES = (
    "manifest_missing_worker_export_signature_fails_before_dispatch",
    "manifest_wrong_worker_export_signature_fails_before_dispatch",
)
INTEROP_BUILDER_IMPORT_CASES = (
    "capability_builder_session_import_profile_reaches_child_import",
    "open_session_with_imports_overrides_builder_imports",
)
INTEROP_FOREIGN_IMPORT_CASES = {
    "manifest_capability_import_workspace_root_imports_external_workspace_only": (
        "admit-fixed-no-exports",
        (2, 2),
    ),
    "pooled_manifest_capability_reuses_external_import_workspace_session": (
        "authorize-fixed-shape",
        (1, 1),
    ),
    "external_import_workspace_root_preserves_timeout_and_fatal_errors": (
        "authorize-fixed-shape",
        (2, 2),
    ),
}
INTEROP_LEGACY_CASE = (
    "no_manifest_worker_session_rejects_worker_command_as_checked_binding"
)
INTEROP_TESTS = (
    *STREAM_TESTS,
    *((target, name) for name, (target, _) in PLANNER_SHAPE_TESTS.items()),
    ("import_planning", IMPORT_PLANNING_CASE),
    *INTEROP_METADATA_TESTS,
    *(("pool", name) for name in POOL_BATCH_GRANTS),
    ("pool", "distinct_session_key_respects_fixed_pool_limit"),
    *(("metadata_doctor", name) for name in INTEROP_DOCTOR_EVENTS),
    *(
        ("capability_builder", name)
        for name in (*INTEROP_SIGNATURE_CASES, *INTEROP_BUILDER_IMPORT_CASES)
    ),
    *(("capability_builder", name) for name in INTEROP_FOREIGN_IMPORT_CASES),
    ("capability_builder", INTEROP_LEGACY_CASE),
    *(("lean_dup_shaped_fixture", name) for name in SDK_SHAPE_GRANTS_BY_WORKER),
)


STREAM_READY_REPLACEMENT = {
    "typed_streaming_command_row_decode_error_names_stream_and_sequence",
    "row_sink_cancellation_cycles_child_and_invalidates_session",
    "request_timeout_cycles_child_and_invalidates_session",
    "cancellation_during_backpressure_invalidates_lease",
}

STREAM_REACQUIRE = {
    "child_fatal_exit_invalidates_lease_and_next_acquire_replaces_worker",
    "cancellation_invalidates_current_lease_but_future_lease_can_continue",
    "timeout_invalidates_current_lease_but_future_lease_can_continue",
}


def stream_package_root():
    return TARGET / "metadata-qualification/stream-package"


def stream_cargo_patch():
    host = Path("crates/lean-rs-host")
    return f'patch.{json.dumps((SDK / host).as_uri())}.lean-rs-host.path={json.dumps(str(stream_package_root() / host))}'


def prepare_stream_package(release, reference):
    root = stream_package_root()
    require(not os.path.lexists(root.parent / "interop-stream"), "previous stream cohort has not closed")
    require(root.parent.resolve(strict=True) == root.parent, "aliased stream package parent")
    root.mkdir(mode=0o700)
    owner = root.stat()
    host = Path("crates/lean-rs-host")
    paths = [str(host / name) for name in ("Cargo.toml", "build.rs", "src", "shims")]
    # Cargo validates even the explicitly declared targets we don't compile here.
    with (SDK / host / "Cargo.toml").open("rb") as stream:
        package = tomllib.load(stream)
    for kind, directory in (("example", "examples"), ("bench", "benches")):
        for target in package.get(kind, []):
            paths.append(str(host / target.get("path", f"{directory}/{target['name']}.rs")))
    paths += ["fixtures/interop-worker", "fixtures/interop-shims/LeanRsInteropConsumer.lean",
              "fixtures/interop-shims/LeanRsInteropConsumer"]
    files = subprocess.check_output(["git", "-C", SDK, "ls-files", "-z", "--", *paths])
    for raw in files.rstrip(b"\0").split(b"\0"):
        rel = Path(os.fsdecode(raw))
        src, dst = SDK / rel, root / rel
        require(src.resolve(strict=True) == src and src.is_file(), f"invalid stream package input: {src}")
        dst.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(src, dst)
    (root / "fixtures/interop-worker").chmod(0o700)
    scratch = root / "tmp"
    scratch.mkdir(mode=0o700)
    env = build_environment(release, scratch)
    config = ["--config", stream_cargo_patch()]
    cargo = ["cargo", "metadata", "--format-version", "1", "--locked", "--offline", *config]
    graph = json.loads(subprocess.check_output(cargo, cwd=APP, env=env))
    require(Path(graph["target_directory"]) == TARGET, "stream worker must reuse the workspace Cargo target")
    selected = [p for p in graph["packages"] if p["name"] == "lean-rs-host"]
    require(
        len(selected) == 1 and Path(selected[0]["manifest_path"]) == root / host / "Cargo.toml",
        "fixture worker did not select the isolated host package",
    )
    subprocess.run(["cargo", "build", "--locked", "--offline", "-j1", *config, "--bin", "prepare"],
                   cwd=APP, env=env, check=True, stdout=sys.stderr)
    manifest = Path(subprocess.check_output([TARGET / "debug/prepare", root, "--stream-shims"],
                                            cwd=APP, env=env, text=True).strip())
    # Only the package root changes. Compare the full SDK manifest, including signatures and toolchain.
    directory = host / "shims/lean-rs-host-shims/.lake/lean-rs-generated/lean-4.36.0/lean-rs-build-cache"
    require(reference.parent == SDK / directory and reference.resolve(strict=True) == reference,
            "select the canonical SDK host manifest")
    require(manifest.parent == root / directory and manifest.resolve(strict=True) == manifest,
            "stream worker prepared a different host package")
    expected = json.loads(reference.read_bytes())
    actual = json.loads(manifest.read_bytes())
    # Rebase only path fields owned by the two bundled Lake packages.
    def original(path):
        return str(SDK / Path(path).relative_to(root))
    contract = {**actual, "primary_dylib": original(actual["primary_dylib"]),
                "search_dirs": [original(d) for d in actual["search_dirs"]],
                "dependencies": [{**d, "dylib_path": original(d["dylib_path"])} for d in actual["dependencies"]]}
    require(contract == expected, "isolated host manifest differs from the selected SDK contract")
    pins = []
    for path in (actual["primary_dylib"], *(d["dylib_path"] for d in actual["dependencies"])):
        image = Path(path)
        require(image.resolve(strict=True) == image, "aliased isolated shim image")
        identity = image.stat()
        pins.append((path, identity.st_dev, identity.st_ino, digest(image)))
    return owner, manifest, pins


def qualify_stream(
    release,
    profile,
    shim_manifest,
    cases,
    *,
    pinned_selection=None,
    bookended_release=False,
    retain_for_workspace=False,
):
    if pinned_selection is not None:
        require(pinned_selection[0]["shim_manifest_path"] == str(shim_manifest)
                and pinned_selection[0]["shim_manifest_json"] == shim_manifest.read_text(),
                "prepared host manifest changed before stream qualification")
    owner, manifest, isolated_pins = prepare_stream_package(release, shim_manifest)
    image_pins = isolated_pins
    if pinned_selection is not None:
        image_pins = [*pinned_selection[1], *isolated_pins]
    selected = cases or [name for _, name in INTEROP_TESTS]
    regular = [
        name
        for name in selected
        if name not in INTEROP_SIGNATURE_CASES
        and name not in INTEROP_FOREIGN_IMPORT_CASES
        and name != INTEROP_LEGACY_CASE
    ]
    foreign = [name for name in selected if name in INTEROP_FOREIGN_IMPORT_CASES]
    probes = [(name,) for name in selected if name in INTEROP_SIGNATURE_CASES]
    legacy = [INTEROP_LEGACY_CASE] if INTEROP_LEGACY_CASE in selected else []
    cohorts = [group for group in (regular, foreign, *probes, legacy) if group]
    descriptor = None
    for index, cohort in enumerate(cohorts):
        descriptor = run_stream_group(release, profile, manifest, cohort, image_pins=image_pins,
                                      build_provider=index == 0, bookended_release=bookended_release)
    if retain_for_workspace:
        require(descriptor is not None, "workspace tests require a verified stream cohort")
        return json.dumps({
            "prefix": str(release),
            "manifest": descriptor["shim_manifest_path"],
            "manifest_sha256": hashlib.sha256(descriptor["shim_manifest_json"].encode()).hexdigest(),
            "roots": descriptor["artifact_roots"],
            "search_paths": descriptor["shim_search_paths"],
            "images": isolated_pins,
            "package_identity": [owner.st_dev, owner.st_ino],
        }, separators=(",", ":"))
    # A failed cohort retains its package and audit until worker closure can be established.
    remove_owned_state(stream_package_root(), owner)


def verify_provider(image_pins):
    for path, dev, inode, sha256 in image_pins:
        image = Path(path)
        identity = image.stat()
        require(
            image.resolve(strict=True) == image
            and (identity.st_dev, identity.st_ino) == (dev, inode)
            and digest(image) == sha256,
            f"stream build changed a verified shim image: {image}",
        )


def workspace_tests(expression):
    # The named qualifier published this selection only after its workers closed.
    root = stream_package_root()
    selection = json.loads(os.environ["LEAN_RS_TEST_ORDINARY_HOST_SHIMS"])
    with ordinary_state_lock():
        require(root.resolve(strict=True) == root, "aliased workspace shim package")
        owner = root.stat()
        require(owner.st_uid == os.geteuid() and stat.S_IMODE(owner.st_mode) == 0o700
                and [owner.st_dev, owner.st_ino] == selection["package_identity"],
                "workspace shim package ownership changed")
        require(not os.path.lexists(root.parent / "interop-stream"), "stream cohort has not closed")
        manifest = Path(selection["manifest"])
        require(manifest.is_relative_to(root) and manifest.resolve(strict=True) == manifest
                and digest(manifest) == selection["manifest_sha256"], "workspace shim manifest changed")
        require(len(selection["images"]) == 2 and all(Path(pin[0]).is_relative_to(root) for pin in selection["images"]),
                "workspace shim images must belong to the retained package")
        verify_provider(selection["images"])
        # A later invocation must not retry or remove a package left by an interrupted workspace run.
        (root / "workspace-running").mkdir(mode=0o700)
        result = subprocess.run(
            ["cargo", "nextest", "run", "--workspace", "--profile", "ci", "-E", expression],
            cwd=SDK,
        )
        if result.returncode:
            # Keep the package when the test run cannot certify closure.
            raise SystemExit(result.returncode)
        remove_owned_state(root, owner)


def require_no_legacy_provider_shadow(provider):
    require(
        not os.path.lexists(provider / ".lake/build/lib/lean"),
        "legacy interop test needs a clean bundled provider: ordinary .lake/build/lib/lean would shadow the pinned provider",
    )


def build_legacy_interop_project(run, release, state, provider):
    project = state / "legacy"
    project.mkdir(mode=0o700)
    (project / "lean-toolchain").write_text("leanprover/lean4:v4.36.0\n")
    source = json.dumps(str(stream_package_root() / "fixtures/interop-shims"), ensure_ascii=False)
    dependency = json.dumps(str(provider), ensure_ascii=False)
    (project / "lakefile.lean").write_text(
        f"""import Lake
open Lake DSL

package «lean_rs_interop_consumer»

require «lean_rs_interop_shims» from {dependency} with
  (Lean.mkNameMap String).insert `leanRsGeneratedShims "true"

@[default_target]
lean_lib «LeanRsInteropConsumer» where
  srcDir := {source}
  leanOptions := #[
    ⟨`autoImplicit, false⟩,
    ⟨`maxSynthPendingDepth, .ofNat 3⟩,
    ⟨`pp.unicode.fun, true⟩,
  ]
  defaultFacets := #[LeanLib.sharedFacet]
"""
    )
    lake = release / "bin/lake"
    run(
        [
            lake,
            "--no-cache",
            "--no-build",
            "-KleanRsGeneratedShims=true",
            "build",
            "LeanRsInterop:shared",
        ],
        provider,
    )
    run(
        [
            lake,
            "--no-cache",
            "--fail-fast",
            "-R",
            "-KleanRsGeneratedShims=true",
            "build",
            "LeanRsInteropConsumer:shared",
        ],
        project,
    )
    require_no_legacy_provider_shadow(provider)


def run_stream_group(
    release,
    profile,
    shim_manifest,
    cases,
    *,
    image_pins,
    build_provider=True,
    bookended_release=False,
):
    legacy = INTEROP_LEGACY_CASE in cases
    require(not legacy or len(cases) == 1, "legacy interop requires its own seal")
    probes = [name for name in cases if name in INTEROP_SIGNATURE_CASES]
    require(not probes or len(cases) == 1, "each signature probe requires its own seal")
    foreign = any(name in INTEROP_FOREIGN_IMPORT_CASES for name in cases)
    require(
        not foreign or all(name in INTEROP_FOREIGN_IMPORT_CASES for name in cases),
        "foreign imports require their own seal",
    )
    project = stream_package_root() / "fixtures/interop-worker"
    require(project.resolve(strict=True) == project, "aliased stream project")
    require(
        project.stat().st_uid == os.geteuid()
        and project.stat().st_mode & 0o7777 == 0o700,
        "stream project requires an exclusively owned checkout",
    )
    bundle = stream_package_root() / "crates/lean-rs-host/shims"
    host, interop = (
        bundle / name for name in ("lean-rs-host-shims", "lean-rs-interop-shims")
    )
    if legacy:
        require_no_legacy_provider_shadow(interop)
    output = Path(".lake/lean-rs-generated/lean-4.36.0")
    manifest = shim_manifest.resolve(strict=True)
    require(
        manifest == shim_manifest
        and manifest.parent == host / output / "lean-rs-build-cache",
        "select the canonical prepared host manifest",
    )
    with manifest.open("rb") as stream:
        raw = stream.read(64 * 1024 + 1)
    require(len(raw) <= 64 * 1024, "shim manifest exceeds wire bound")
    descriptor = {
        "protocol": 2,
        "shim_manifest_path": str(manifest),
        "shim_manifest_json": raw.decode(),
        "artifact_roots": list(map(str, sorted((host, interop)))),
        "shim_search_paths": [
            str(root / output / "lib/lean") for root in (interop, host)
        ],
    }
    parent = TARGET / "metadata-qualification"
    require(parent == parent.resolve(strict=True), "aliased fixture directory")
    state = parent / "interop-stream"
    state.mkdir(mode=0o700)
    owner = state.stat()
    scratch = state / "tmp"
    scratch.mkdir(mode=0o700)
    audit_dir = state / "app"
    audit_dir.mkdir(mode=0o700)
    events = audit_dir / "native-events.log"
    env = build_environment(release, scratch)
    env["LEAN_RS_PREPARED_HOST_ARTIFACTS"] = json.dumps(descriptor)
    timeout = shutil.which("gtimeout" if platform.system() == "Darwin" else "timeout")
    require(timeout is not None, "GNU timeout is required for the stream check")

    def run(args, cwd):
        space(TARGET)
        subprocess.run(
            [timeout, "--signal=TERM", "--kill-after=15s", "8m", *args],
            cwd=cwd,
            env=ordinary_compile_environment(env, args, bookended_release),
            check=True,
            stdout=sys.stderr,
        )

    # The typed workers have closed and this job holds the qualification lock.
    # The probe and image checks detect changes; they do not block writes.
    verify_provider(image_pins)
    if build_provider and not legacy:
        lake = release / "bin/lake"
        run(
            [lake, "--no-cache", "--no-build", "-KleanRsGeneratedShims=true", "build", "LeanRsInterop:shared"],
            interop,
        )
        run(
            [
                lake,
                "--no-cache",
                "--fail-fast",
                "-R",
                "-KleanRsGeneratedShims=true",
                "build",
                "LeanRsInteropConsumer:shared",
            ],
            project,
        )
    verify_provider(image_pins)
    if legacy:
        build_legacy_interop_project(run, release, state, interop)
        verify_provider(image_pins)
    cargo = ["cargo", "build", "--locked", "--offline", "-j1", "--config", stream_cargo_patch()]
    if probes:
        env["LEAN_RS_TEST_SIGNATURE_PROBE"] = probes[0]
    if foreign:
        env["LEAN_RS_TEST_PREPARE_FOREIGN_WORKSPACE"] = "1"
    run(
        [
            TARGET / "debug/prepare",
            state,
            "--interop-legacy" if legacy else "--interop-stream",
        ],
        APP,
    )
    env.pop("LEAN_RS_TEST_PREPARE_FOREIGN_WORKSPACE", None)
    env.pop("LEAN_RS_TEST_SIGNATURE_PROBE", None)
    record = state / "deployment.json"
    env.update(
        LEAN_RS_METADATA_ORDINARY_TYPED="1",
        LEAN_RS_METADATA_DEPLOYMENT_RECORD=str(record),
        LEAN_RS_METADATA_DEPLOYMENT_SHA256=digest(record),
        LEAN_RS_METADATA_AUDIT_PATH=str(events),
        LEAN_RS_TEST_AUDIT_WORKER_START="1",
    )
    run([*cargo, "--features", "admitted", "--bin", "metadata-worker"], APP)
    worker = TARGET / "debug/metadata-worker"
    launch = {
        "worker": str(worker),
        "worker_sha256": fixture_worker_digest(worker),
        "fixture": str(state / "legacy" if legacy else project),
        "sysroot": str(release),
    }
    if legacy:
        env["LEAN_RS_TEST_DIRECT_INTEROP"] = json.dumps(launch)
    else:
        launch["json_manifest"] = str(state / "worker-stream-capability.json")
        if foreign:
            launch["foreign_workspace"] = str(state / "foreign")
        env["LEAN_RS_TEST_ORDINARY_STREAM"] = json.dumps(launch)
    select_nextest_runner(env, profile)
    selected = {name: target for target, name in INTEROP_TESTS}
    for name in cases or selected:
        target = selected[name]
        env["LEAN_RS_METADATA_STREAM_CASE"] = name
        run(
            [
                "cargo",
                "nextest",
                "run",
                "--locked",
                "--offline",
                "-j1",
                "--no-tests",
                "fail",
                "--retries",
                "0",
                "-p",
                "lean-rs-worker-child",
                "--test",
                target,
                "-E",
                scoped_tests(target, (name,)),
            ],
            SDK,
        )
        counts = (1, 1)
        if name == INTEROP_LEGACY_CASE:
            marker = "admit-fixed-direct-interop"
        elif name in INTEROP_FOREIGN_IMPORT_CASES:
            marker, counts = INTEROP_FOREIGN_IMPORT_CASES[name]
        elif name in INTEROP_SIGNATURE_CASES:
            marker = "authorize-fixed-signature-probe"
        elif name in POOL_BATCH_GRANTS or name in INTEROP_BUILDER_IMPORT_CASES:
            marker = "admit-fixed-no-exports"
        elif name in INTEROP_DOCTOR_EVENTS:
            marker = INTEROP_DOCTOR_EVENTS[name]
        elif name in SDK_SHAPE_GRANTS_BY_WORKER or name in PLANNER_SHAPE_TESTS:
            marker = "authorize-fixed-shape"
        elif name in INTEROP_METADATA_CASES:
            marker = "authorize-fixed-metadata"
        elif name in {
            IMPORT_PLANNING_CASE,
            "typed_json_command_decode_error_is_typed",
            "distinct_session_key_respects_fixed_pool_limit",
        }:
            marker = "authorize-fixed-json"
        else:
            marker = "authorize-fixed-stream"
        grant_event = f"{marker}-{name}".encode()
        grant_order = None
        if name in POOL_BATCH_GRANTS:
            grant_order = (grant_event,) + (BATCH_GRANT,) * POOL_BATCH_GRANTS[name]
            counts = (1, 1 + POOL_BATCH_GRANTS[name])
        elif name == IMPORT_PLANNING_CASE:
            counts = (3, 3)
        elif name in PLANNER_SHAPE_TESTS:
            _, shape_grants = PLANNER_SHAPE_TESTS[name]
            counts = (len(shape_grants), sum(shape_grants))
        elif shape_grants := SDK_SHAPE_GRANTS_BY_WORKER.get(name):
            counts = (len(shape_grants), sum(shape_grants))
        elif name in STREAM_READY_REPLACEMENT:
            counts = (2, 1)
        elif name in STREAM_REACQUIRE:
            counts = (2, 2)
        check_typed_children_closed(
            typed_audit(events),
            case=name,
            expected_counts=(counts,),
            grant_event=grant_event,
            grant_order=grant_order,
        )
        events.unlink()
    remove_owned_state(state, owner)
    return descriptor


def ordinary_host_shim_selection(release, selected):
    image_pins = []
    for name in ("host_image", "interop_image"):
        image = selected[name]
        require(Path(image).is_relative_to(SDK), f"invalid {name} selection")
        image_pins.append([image, *selected["files"][image]])
    return json.dumps(
        {
            "prefix": str(release),
            "manifest": selected["shim_manifest"],
            "manifest_sha256": hashlib.sha256(
                selected["shim_json"].encode()
            ).hexdigest(),
            "roots": selected["shim_roots"],
            "search_paths": selected["shim_paths"],
            "images": image_pins,
        },
        separators=(",", ":"),
    ), image_pins


def adapter_host_inputs(release, record, pinned_digest):
    with record.open("rb") as stream:
        data = stream.read(16 * 1024**2 + 1)
    require(len(data) <= 16 * 1024**2, "typed record exceeds fixture bound")
    require(hashlib.sha256(data).hexdigest() == pinned_digest, "typed record changed")
    selected = json.loads(data)
    fixture = SDK / "fixtures/lean"
    require(Path(selected["project"]) == fixture, "adapter selects a different project")
    native = "dylib" if platform.system() == "Darwin" else "so"
    image = fixture / ".lake/build/lib" / f"liblean__rs__fixture_LeanRsFixture.{native}"
    require(
        Path(selected["consumer_image"]) == image,
        "adapter selects a different fixture image",
    )
    dev, ino, digest = selected["files"][str(image)]
    shims, _ = ordinary_host_shim_selection(release, selected)
    return {
        "LEAN_RS_TEST_SCALAR_PRIVATE_JOB": "1",
        "LEAN_RS_TEST_SCALAR_FIXTURE": json.dumps([str(fixture), dev, ino]),
        "LEAN_RS_PROFILE_FIXTURE_SHA256": digest,
        "LEAN_RS_TEST_ORDINARY_HOST_SHIMS": shims,
    }


def qualify_typed_group(parent, release, profile, run, env, cargo, descriptor):
    state = parent / "typed-session"
    state.mkdir(mode=0o700)
    owner = state.stat()
    scratch = state / "tmp"
    scratch.mkdir(mode=0o700)
    env.clear()
    env.update(
        build_environment(release, scratch), LEAN_RS_PREPARED_HOST_ARTIFACTS=descriptor
    )
    run([TARGET / "debug/prepare", state, "--typed-session"], APP)
    record = state / "deployment.json"
    require(
        record == record.resolve(strict=True) and record.is_file(),
        "missing exact typed fixture record",
    )
    manifest = state / "worker-json-capability.json"
    require(
        manifest == manifest.resolve(strict=True) and manifest.is_file(),
        "missing sealed typed JSON capability manifest",
    )
    audit_dir = state / "app"
    audit_dir.mkdir(mode=0o700)
    events = audit_dir / "native-events.log"
    env.update(
        LEAN_RS_METADATA_ORDINARY_TYPED="1",
        LEAN_RS_METADATA_DEPLOYMENT_RECORD=str(record),
        LEAN_RS_METADATA_DEPLOYMENT_SHA256=digest(record),
        LEAN_RS_METADATA_AUDIT_PATH=str(events),
    )
    run([*cargo, "--features", "admitted", "--bin", "metadata-worker"], APP)
    worker = TARGET / "debug/metadata-worker"
    selection = json.dumps(
        {
            "worker": str(worker),
            "worker_sha256": fixture_worker_digest(worker),
            "fixture": str(SDK / "fixtures/lean"),
            "json_manifest": str(manifest),
            "sysroot": str(release),
            "prepared": json.loads(descriptor),
        },
        separators=(",", ":"),
    )
    require(
        len(selection.encode()) <= 64 * 1024, "typed selection exceeds launch limit"
    )
    env["LEAN_RS_TEST_ORDINARY_TYPED_SESSION"] = selection
    # These direct-host pins are compiled into the parity test and checked at runtime.
    env.update(
        adapter_host_inputs(release, record, env["LEAN_RS_METADATA_DEPLOYMENT_SHA256"])
    )
    select_nextest_runner(env, profile)

    def nextest(*targets, selection):
        run(
            [
                "cargo",
                "nextest",
                "run",
                "--locked",
                "--offline",
                "-j1",
                "--no-tests",
                "fail",
                "--retries",
                "0",
                "-p",
                "lean-rs-worker-child",
                *[arg for target in targets for arg in ("--test", target)],
                "-E",
                selection,
            ],
            SDK,
        )

    # Keep one audit under the existing cap. Clear it only after all recorded
    # workers have closed; a failed run leaves its state untouched.
    nextest("typed_session", "session_reuse", selection=ordinary_app_tests())
    check_typed_children_closed(typed_audit(events))
    events.unlink()
    env["LEAN_RS_TEST_AUDIT_WORKER_START"] = "1"
    for target, name, expected_counts, grant_event in ORDINARY_AUDITED_TESTS:
        if name in ORDINARY_WIDE_REUSE_CASES:
            env["LEAN_RS_TEST_WIDE_REUSE_CASE"] = name
        if name in ORDINARY_STALENESS_CASES:
            env["LEAN_RS_TEST_STALENESS_PROFILE"] = "1"
        if name == ORDINARY_MODULE_SYNTAX_CASE:
            env["LEAN_RS_TEST_MODULE_SYNTAX_CASE"] = name
        if name in ORDINARY_ADAPTER_CASES:
            env["LEAN_RS_TEST_ADAPTER_CASE"] = name
        nextest(target, selection=scoped_tests(target, (name,)))
        env.pop("LEAN_RS_TEST_ADAPTER_CASE", None)
        env.pop("LEAN_RS_TEST_MODULE_SYNTAX_CASE", None)
        env.pop("LEAN_RS_TEST_WIDE_REUSE_CASE", None)
        env.pop("LEAN_RS_TEST_STALENESS_PROFILE", None)
        check_typed_children_closed(
            typed_audit(events),
            case=name,
            expected_counts=expected_counts,
            grant_event=grant_event,
        )
        events.unlink()
    remove_owned_state(state, owner)


def check_host_builder_children_closed(
    data, expected, *, prefix=(b"worker-started", b"verified-deployment")
):
    by_pid = {}
    for line in data.splitlines():
        raw, event = line.split(b" ", 1)
        require(raw.isdigit() and int(raw) > 0, "invalid host builder PID")
        by_pid.setdefault(int(raw), []).append(event)
    require(
        list(by_pid.values()) == [[*prefix, *events] for events in expected],
        "wrong worker count, custody or ordered imports for host builder",
    )
    for pid in by_pid:
        try:
            os.kill(pid, 0)
        except ProcessLookupError:
            continue
        raise ValueError(
            f"host builder worker PID {pid} remains; keeping fixture state"
        )


def launch_host_builder(case, mode, project, state, release, profile, run, env, cargo):
    run([TARGET / "debug/prepare", state, mode], APP)
    record = state / "deployment.json"
    require(
        record == record.resolve(strict=True) and record.is_file(),
        "missing exact host builder record",
    )
    env.update(
        LEAN_RS_METADATA_BROKEN_IMPORT="1",
        LEAN_RS_METADATA_DEPLOYMENT_RECORD=str(record),
        LEAN_RS_METADATA_DEPLOYMENT_SHA256=digest(record),
    )
    if case == DIRECT_BROKEN_SHARED_TEST:
        select_nextest_runner(env, profile)
        return record
    if case != BROKEN_IMPORT_TEST:
        env.update(
            LEAN_RS_METADATA_AUDIT_PATH=str(state / "native-events.log"),
            LEAN_RS_TEST_AUDIT_WORKER_START="1",
            LEAN_RS_TEST_HOST_BUILDER_CASE=case,
        )
    run([*cargo, "--features", "admitted", "--bin", "metadata-worker"], APP)
    worker = TARGET / "debug/metadata-worker"
    selection = {
        "project": str(state / project),
        "worker": str(worker),
        "sysroot": str(release),
        "worker_sha256": fixture_worker_digest(worker),
    }
    if case == BROKEN_IMPORT_TEST:
        env["LEAN_RS_TEST_BROKEN_IMPORT"] = json.dumps(selection)
    elif case == GOOD_WORKER_TEST:
        selected = json.loads(record.read_bytes())
        env["LEAN_RS_PREPARED_HOST_ARTIFACTS"] = json.dumps(
            {
                "protocol": 2,
                "shim_manifest_path": selected["shim_manifest"],
                "shim_manifest_json": selected["shim_json"],
                "artifact_roots": selected["shim_roots"],
                "shim_search_paths": selected["shim_paths"],
            }
        )
        env["LEAN_RS_TEST_ORDINARY_SCALAR"] = json.dumps(
            {"fixture": selection.pop("project"), **selection}
        )
    else:
        env["LEAN_RS_TEST_HOST_BUILDER"] = json.dumps({"case": case, **selection})
    select_nextest_runner(env, profile)
    run(
        [
            "cargo",
            "nextest",
            "run",
            "--locked",
            "--offline",
            "-j1",
            "--no-tests",
            "fail",
            "--retries",
            "0",
            "-p",
            "lean-rs-worker-child",
            "--test",
            "session_adapter" if case == GOOD_WORKER_TEST else "capability_builder",
            "-E",
            f"test(={case})",
        ],
        SDK,
    )
    return record


def qualify_direct_builder_case(case, state, run, env):
    name, marker = DIRECT_BUILDER_TESTS[case]
    events = state / "direct-events.log"
    env["LEAN_RS_METADATA_AUDIT_PATH"] = str(events)
    run(
        [
            "cargo",
            "nextest",
            "run",
            "--locked",
            "--offline",
            "-j1",
            "--no-tests",
            "fail",
            "--retries",
            "0",
            "--features",
            "admitted",
            "--bin",
            "control",
            "-E",
            f"test(={name})",
        ],
        APP,
    )
    prefix = (b"verified-deployment",) if case == DIRECT_OPTIONAL_WHNF_TEST else ()
    check_host_builder_children_closed(typed_audit(events), ((marker,),), prefix=prefix)
    return name


def qualify_host_builder_cases(parent, release, profile, run, env, cargo, descriptor):
    state = parent / "broken-import"
    direct_tests = []
    for case, mode, project, imports in (
        *HOST_BUILDER_CASES,
        (
            DIRECT_BROKEN_SHARED_TEST,
            "--direct-host-broken-shared",
            "lean-broken-shared",
            (),
        ),
    ):
        # Each graph has the original sources, its own seal and a checked close.
        # Refuse an incomplete previous run before touching this state.
        state.mkdir(mode=0o700)
        owner = state.stat()
        scratch = state / "tmp"
        scratch.mkdir(mode=0o700)
        env.clear()
        env.update(build_environment(release, scratch), LEAN_RS_PREPARED_HOST_ARTIFACTS=descriptor)
        launch_host_builder(
            case, mode, project, state, release, profile, run, env, cargo
        )
        if case != DIRECT_BROKEN_SHARED_TEST:
            check_host_builder_children_closed(
                typed_audit(state / "native-events.log"), imports
            )
        if case in DIRECT_BUILDER_TESTS:
            direct_tests.append(qualify_direct_builder_case(case, state, run, env))
        remove_owned_state(state, owner)
    env.clear()
    return tuple(direct_tests)


def qualify_optional_whnf(release, profile, run, env):
    parent = APP / "target"
    parent.mkdir(exist_ok=True)
    require(parent == parent.resolve(strict=True), "aliased optional WHNF parent")
    state = parent / "optional-whnf"
    state.mkdir(mode=0o700)
    owner = state.stat()
    scratch = state / "tmp"
    scratch.mkdir(mode=0o700)
    runpy.run_path(str(APP / "seal.py"))["stage_optional_shims"](SDK, state)
    env.clear()
    env.update(build_environment(release, scratch))
    run([TARGET / "debug/prepare", state, "--optional-whnf"], APP)
    require(
        sum(path.lstat().st_size for path in (state / "shims").rglob("*"))
        <= 64 * 1024**2,
        "optional WHNF shim fixture exceeded its output bound",
    )
    record = state / "deployment.json"
    require(
        record == record.resolve(strict=True) and record.is_file(),
        "missing exact optional WHNF deployment",
    )
    env.update(
        LEAN_RS_METADATA_DEPLOYMENT_RECORD=str(record),
        LEAN_RS_METADATA_DEPLOYMENT_SHA256=digest(record),
    )
    select_nextest_runner(env, profile)
    name = qualify_direct_builder_case(DIRECT_OPTIONAL_WHNF_TEST, state, run, env)
    remove_owned_state(state, owner)
    env.clear()
    return (name,)


SCALAR_ENTRY_TESTS = (
    ("protocol", "fixture_capability_loads_and_exported_call_succeeds", 2),
    ("supervisor", "public_fixture_export_call_succeeds", 2),
    ("protocol", "lean_internal_panic_kills_only_child", 1),
    ("supervisor", "child_crash_is_typed_and_parent_survives", 1),
)


def qualify_scalar_entry(release, profile, run, env, cargo, descriptor):
    parent = APP / "target"
    parent.mkdir(exist_ok=True)
    require(parent.resolve(strict=True) == parent, "aliased scalar fixture parent")
    state = parent / "scalar-entry"
    state.mkdir(mode=0o700)
    owner = state.stat()
    scratch = state / "tmp"
    scratch.mkdir(mode=0o700)
    project = state / "lean"
    project.mkdir(mode=0o700)
    sources = subprocess.check_output(
        ["git", "ls-files", "-z", "fixtures/lean"], cwd=SDK
    ).split(b"\0")
    require(2 <= len(sources) <= 33, "unexpected scalar source count")
    for raw in filter(None, sources):
        source = SDK / os.fsdecode(raw)
        relative = source.relative_to(SDK / "fixtures/lean")
        if (
            relative.suffix not in (".lean", ".json")
            and relative.name != "lean-toolchain"
        ):
            continue
        require(
            source.resolve(strict=True) == source
            and source.is_file()
            and source.stat().st_size <= 256 * 1024,
            "unapproved scalar source",
        )
        tracked = subprocess.check_output(
            ["git", "show", f"HEAD:{source.relative_to(SDK)}"], cwd=SDK
        )
        require(
            tracked == source.read_bytes(), "scalar source differs from tracked input"
        )
        destination = project / relative
        destination.parent.mkdir(parents=True, exist_ok=True)
        destination.write_bytes(tracked)
    env.clear()
    env.update(
        build_environment(release, scratch), LEAN_RS_PREPARED_HOST_ARTIFACTS=descriptor
    )
    run([TARGET / "debug/prepare", state, "--scalar-entry"], APP)
    record = state / "deployment.json"
    events = state / "native-events.log"
    env.update(
        LEAN_RS_TEST_SCALAR_ENTRY="1",
        LEAN_RS_METADATA_DEPLOYMENT_RECORD=str(record),
        LEAN_RS_METADATA_DEPLOYMENT_SHA256=digest(record),
        LEAN_RS_METADATA_AUDIT_PATH=str(events),
        LEAN_RS_TEST_AUDIT_WORKER_START="1",
    )
    run([*cargo, "--features", "admitted", "--bin", "metadata-worker"], APP)
    worker = TARGET / "debug/metadata-worker"
    env["LEAN_RS_TEST_ORDINARY_SCALAR"] = json.dumps(
        {
            "worker": str(worker),
            "worker_sha256": fixture_worker_digest(worker),
            "fixture": str(project),
            "sysroot": str(release),
        },
        separators=(",", ":"),
    )
    # Nextest injects the test crate's Cargo OUT_DIR. The fixture is not a
    # build script output: all callers must select the same project cache.
    select_nextest_runner(
        env, {**profile, "nextest_runner": profile["nextest_runner"] + " -u OUT_DIR"}
    )
    for target, case, grants in SCALAR_ENTRY_TESTS:
        run(
            [
                "cargo",
                "nextest",
                "run",
                "--locked",
                "--offline",
                "-j1",
                "--no-tests",
                "fail",
                "--retries",
                "0",
                "-p",
                "lean-rs-worker-child",
                "--test",
                target,
                "-E",
                scoped_tests(target, (case,)),
            ],
            SDK,
        )
        check_typed_children_closed(
            typed_audit(events),
            case=case,
            expected_counts=((1, grants),),
            grant_event=b"authorize-scalar-fixture",
        )
        events.unlink()
    remove_owned_state(state, owner)
    env.clear()


def ordinary_compile_environment(env, command, bookended_release):
    if bookended_release and command[:2] == ["cargo", "build"]:
        return {**env, "LEAN_RS_METADATA_BOOKENDED_RELEASE": "1"}
    return env


def qualify_broken_import(
    release,
    profile,
    *,
    emit_host_selection=False,
    run_typed=False,
    run_stream=False,
    bookended_release=False,
    retain_for_workspace=False,
):
    """Run one ordinary SDK worker test, without the downstream host helper."""
    require(TARGET == TARGET.resolve(strict=True), "aliased SDK target")
    parent = TARGET / "metadata-qualification"
    require(parent == parent.resolve(strict=True), "aliased fixture directory")
    timeout = (
        shutil.which("gtimeout")
        if platform.system() == "Darwin"
        else shutil.which("timeout")
    )
    require(timeout is not None, "GNU timeout is required for the broken-import check")
    if run_typed:
        require(
            not os.path.lexists(APP / "target/scalar-entry"),
            "scalar fixture state already exists",
        )
        require(
            not os.path.lexists(APP / "target/optional-whnf"),
            "optional WHNF state already exists",
        )
        require(emit_host_selection, "--run-typed requires --emit-host-selection")
        typed = parent / "typed-session"
        require(
            not os.path.lexists(typed), f"typed fixture state already exists: {typed}"
        )
    require(not run_stream or run_typed, "stream tests require the typed fixture first")
    check_tracked_fixture_sources()
    space(TARGET)
    state = parent / "broken-import"
    state.mkdir(mode=0o700)  # Refuse a previous or uncertain invocation.
    owner = state.stat()
    scratch = state / "tmp"
    scratch.mkdir(mode=0o700)
    env = build_environment(release, scratch)

    def run(command, cwd):
        space(TARGET)
        subprocess.run(
            [timeout, "--signal=TERM", "--kill-after=15s", "8m", *command],
            cwd=cwd,
            env=ordinary_compile_environment(env, command, bookended_release),
            check=True,
            **({"stdout": sys.stderr} if emit_host_selection else {}),
        )

    cargo = ["cargo", "build", "--locked", "--offline", "-j1"]
    run([*cargo, "--bin", "prepare"], APP)
    record = launch_host_builder(
        BROKEN_IMPORT_TEST,
        "--broken-import",
        "lean",
        state,
        release,
        profile,
        run,
        env,
        cargo,
    )

    # The named test confirms both workers were reaped. On a failure or
    # timeout, keep this state and refuse further attempts pending closure.
    if emit_host_selection:
        with record.open("rb") as stream:
            data = stream.read(16 * 1024**2 + 1)
        require(len(data) <= 16 * 1024**2, "broken-import record exceeds fixture bound")
        require(
            hashlib.sha256(data).hexdigest()
            == env["LEAN_RS_METADATA_DEPLOYMENT_SHA256"],
            "broken-import selection changed after verification",
        )
        selected = json.loads(data)
        manifest = Path(selected["shim_manifest"])
        host_selection, image_pins = ordinary_host_shim_selection(release, selected)
        typed_descriptor = json.dumps(
            {
                "protocol": 2,
                "shim_manifest_path": str(manifest),
                "shim_manifest_json": selected["shim_json"],
                "artifact_roots": selected["shim_roots"],
                "shim_search_paths": selected["shim_paths"],
            },
            separators=(",", ":"),
        )
        require(
            len(typed_descriptor.encode()) <= 64 * 1024,
            "prepared shim descriptor exceeds wire bound",
        )
    remove_owned_state(state, owner)
    direct_tests = ()
    if run_typed:
        qualify_typed_group(parent, release, profile, run, env, cargo, typed_descriptor)
        direct_tests = qualify_host_builder_cases(
            parent, release, profile, run, env, cargo, typed_descriptor
        )
        qualify_scalar_entry(release, profile, run, env, cargo, typed_descriptor)
        direct_tests += qualify_optional_whnf(release, profile, run, env)
    if run_stream:
        isolated_selection = qualify_stream(
            release,
            profile,
            manifest,
            None,
            pinned_selection=(json.loads(typed_descriptor), image_pins),
            bookended_release=bookended_release,
            retain_for_workspace=retain_for_workspace,
        )
        if retain_for_workspace:
            host_selection = isolated_selection
    if emit_host_selection:
        print("LEAN_RS_TEST_ORDINARY_HOST_SHIMS=" + host_selection)
        print("LEAN_RS_PREPARED_HOST_ARTIFACTS=" + typed_descriptor)
        if run_typed:
            typed = " | ".join(
                [
                    ordinary_app_tests(),
                    *(
                        scoped_tests(target, (name,))
                        for target, name, _, _ in ORDINARY_AUDITED_TESTS
                    ),
                ]
            )
            broken = scoped_tests(
                "capability_builder",
                (
                    BROKEN_IMPORT_TEST,
                    *(
                        case
                        for case, *_ in HOST_BUILDER_CASES
                        if case != GOOD_WORKER_TEST
                    ),
                ),
            )
            completed = [
                broken,
                typed,
                *(
                    scoped_tests(target, (name,))
                    for target, name, _ in SCALAR_ENTRY_TESTS
                ),
            ]
            if run_stream:
                completed.extend(
                    scoped_tests(target, (name,)) for target, name in INTEROP_TESTS
                )
            excluded = f"package(=lean-rs-worker-child) & ({' | '.join(completed)})"
            if DIRECT_GOOD_TEST in direct_tests:
                good = scoped_tests("session_adapter", (GOOD_WORKER_TEST,))
                excluded = f"({excluded}) | (package(=lean-rs-worker-child) & {good})"
            if direct_tests:
                host = scoped_tests("host_services", direct_tests)
                excluded = f"({excluded}) | (package(=lean-rs-host) & {host})"
            print(f"LEAN_RS_TEST_REMAINING_FILTER=not ({excluded})")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--writer", type=Path)
    parser.add_argument(
        "--writer-sha256", help="independently reviewed digest of this local helper"
    )
    modes = parser.add_mutually_exclusive_group()
    modes.add_argument(
        "--disposable-clean",
        action="store_true",
        help="prepare or consume a one-use typed deployment to check guarded Lake clean",
    )
    modes.add_argument(
        "--typed-session",
        action="store_true",
        help="qualify the fixed LeanRsFixture Nat.add and scalar Boolean selectors",
    )
    modes.add_argument(
        "--verify-declaration",
        action="store_true",
        help="qualify four reviewed single-declaration requests on the typed fixture",
    )
    commands = parser.add_subparsers(dest="command", required=True)
    commands.add_parser(
        "prepare", help="build and seal a new deployment, then print candidate hashes"
    )
    broken = commands.add_parser(
        "broken-import",
        help="run the private SDK broken-import fixture without a writer helper",
    )
    broken.add_argument(
        "--emit-host-selection",
        action="store_true",
        help="emit checked environment assignments for these prepared shims",
    )
    broken.add_argument(
        "--run-typed", action="store_true", help="also run the fixed typed SDK tests"
    )
    broken.add_argument("--workspace-shims", action="store_true",
                        help="retain the checked stream package for workspace-tests")
    workspace = commands.add_parser("workspace-tests", help="run workspace Nextest with the retained shim package")
    workspace.add_argument("expression", help="Nextest filter from the named qualifier")
    stream = commands.add_parser(
        "interop-stream", help="qualify the fixed-export stream tests"
    )
    stream.add_argument("shim_manifest", type=Path)
    stream.add_argument(
        "--case", action="append", choices=[name for _, name in INTEROP_TESTS]
    )
    run = commands.add_parser(
        "run", help="execute the selected tests against a reviewed deployment"
    )
    run.add_argument(
        "--unknown-declaration",
        action="store_true",
        help="with --typed-session, run only the unknown-declaration lookup",
    )
    run.add_argument(
        "--pool-validator-recovery",
        action="store_true",
        help="with the ordinary metadata fixture, run only the two-launch pool validator recovery check",
    )
    run.add_argument("deployment", type=Path)
    run.add_argument("--worker-sha256", required=True)
    run.add_argument("--control-sha256", required=True)
    run.add_argument("--deployment-sha256", required=True)
    args = parser.parse_args()
    if args.command == "workspace-tests":
        try:
            workspace_tests(args.expression)
        except (OSError, ValueError, KeyError) as error:
            parser.exit(1, f"workspace shim qualification failed: {error}\n")
        return
    if args.command == "broken-import":
        if args.writer is not None or args.writer_sha256 is not None:
            parser.error("broken-import does not use a writer helper")
        if args.run_typed and not args.emit_host_selection:
            parser.error("--run-typed requires --emit-host-selection")
        if args.workspace_shims and not args.run_typed:
            parser.error("--workspace-shims requires --run-typed")
    elif args.command == "interop-stream":
        if args.writer is not None or args.writer_sha256 is not None:
            parser.error("interop-stream does not use a writer helper")
    elif args.writer is None or args.writer_sha256 is None:
        parser.error("prepare and run require --writer and --writer-sha256")
    if args.command == "run" and args.unknown_declaration and not args.typed_session:
        parser.error("--unknown-declaration requires --typed-session")
    if args.command == "run" and args.pool_validator_recovery and args.typed_session:
        parser.error("--pool-validator-recovery requires the ordinary metadata fixture")
    if (
        args.command == "run"
        and args.pool_validator_recovery
        and args.unknown_declaration
    ):
        parser.error("--pool-validator-recovery conflicts with --unknown-declaration")
    if (
        args.typed_session
        or args.disposable_clean
        or args.verify_declaration
        or (args.command == "run" and args.unknown_declaration)
    ):
        parser.error(
            "typed, verification and disposable-clean selectors are temporarily unavailable; ordinary metadata only"
        )
    try:
        refuse_inherited_loader_overrides()
        profile = verified_platform(
            broken_import=args.command in ("broken-import", "interop-stream")
        )
        release = installed_lean_release(profile)
        if args.command == "interop-stream":
            with ordinary_state_lock():
                run_ordinary_with_release_bookends(
                    release,
                    lambda: qualify_stream(
                        release,
                        profile,
                        args.shim_manifest,
                        args.case,
                        bookended_release=True,
                    ),
                )
            return
        if args.command == "broken-import":
            with ordinary_state_lock():
                run_ordinary_with_release_bookends(
                    release,
                    lambda: qualify_broken_import(
                        release,
                        profile,
                        emit_host_selection=args.emit_host_selection,
                        run_typed=args.run_typed,
                        run_stream=args.run_typed,
                        bookended_release=True,
                        retain_for_workspace=args.workspace_shims,
                    ),
                    emit_host_selection=args.emit_host_selection,
                )
            return
        reviewed_writer(args.writer, args.writer_sha256)
        writer = args.writer.resolve(strict=True)
        typed = args.typed_session or args.disposable_clean or args.verify_declaration
        with ordinary_state_lock():
            if args.command == "prepare":
                provision(
                    writer,
                    args.writer_sha256,
                    release,
                    typed,
                    args.disposable_clean,
                    args.verify_declaration,
                )
            else:
                qualify(
                    args.deployment,
                    writer,
                    args.writer_sha256,
                    args.worker_sha256,
                    args.deployment_sha256,
                    typed,
                    args.disposable_clean,
                    args.verify_declaration,
                    unknown_declaration=args.unknown_declaration,
                    pool_validator_recovery=args.pool_validator_recovery,
                    release=release,
                    approved_control=args.control_sha256,
                    profile=profile,
                )
    except (
        OSError,
        RuntimeError,
        ValueError,
        KeyError,
        subprocess.CalledProcessError,
    ) as error:
        parser.exit(1, f"metadata qualification failed: {error}\n")


if __name__ == "__main__":
    main()
