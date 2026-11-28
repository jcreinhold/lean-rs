# Architecture

## Goal

lean-rs runs Lean 4 from Rust. It finds a toolchain, asks Lake for paths, loads Lean libraries, imports modules,
elaborates, evaluates and kernel-checks, and runs that work in worker children that exit when their parent dies. Lean
decides what a term means; lean-rs decides how Lean runs; Rust never recomputes a fact that Lean owns.

The public API is what the tools built on lean-rs use. Add a public item only for a caller that uses it, and write no
guide for other embedders. Items that only other lean-rs crates use stay `pub`, since Rust has nothing between
`pub(crate)` and `pub`; their docs say what they do, not how an outside embedder would use them.

| Requirement | Meaning | Checked by |
| --- | --- | --- |
| Supported toolchains | A build accepts only a toolchain whose `lean.h` digest is in the window, and every required symbol resolves on each supported release | `version_window.rs` and `linkage.rs`, on every release in a `full_matrix` CI run |
| Faithful ABI | Rust's mirrors match `lean.h`, reference counts balance, and each conversion round-trips | `repr.rs`, `refcount.rs` and the ABI round-trip tests |
| Memory safety | Safe code cannot reach an object's layout, a reference count or a raw symbol, and each `unsafe` says why it is sound | `safety_grep.rs`, the compile-fail suites, and review of each `unsafe` outside `lean-rs-sys` |
| Contained | Lean work that can abort or exhaust memory runs in a child; the child exits when its parent dies; the parent reaps each child it starts | `parent_death::busy_worker_exits_when_its_parent_is_killed`, `lean_internal_panic_kills_only_child` and the `process.rs` tests |
| One outcome | Each request ends in exactly one terminal outcome, and output from an old child or an old request never completes a new one | the `conformance_*` tests |
| Warm | Requests on the same imports reuse a loaded environment, and a reused session carries no state from the last one | `session_reuse.rs` |
| Bounded | Replies, diagnostics, frames, heartbeats, waits and memory each have a stated limit that the code enforces | the budget, frame and deadline tests below |
| Lake-native | Lake decides what to build, where outputs live and whether they are current; outside a listed set of workflows, scripts, fixtures and tests, nothing reads Lake's files or builds their paths | the CI step "Ask Lake, never read its files", which gives each exception its reason |
| Deliberate API | The public API changes only on purpose, and each crate packages and builds its docs without Lean | the public-API diff and `scripts/check_package_docsrs.py` |

Not goals: platforms other than x86_64 Linux and Apple Silicon macOS; a general-purpose SDK; resetting Lean inside one
process, since only process exit resets it; locks that order builds and checks across processes; ranking results. The
supported Lean releases are in the [version matrix](../README.md#version-matrix).

## Who owns what

| Crate | Owns | Never holds |
| --- | --- | --- |
| `lean-rs-abi` | The supported window, the required symbols, `lean.h` layout constants | A build script, an `extern`, a link directive, a probe of an installed toolchain |
| `lean-toolchain` | Finding a toolchain and its identity; asking Lake and reading its answers; artifact manifests; link diagnostics; process groups; source packages | Lake's files or output layout; locks between processes, except one per source-package cache entry |
| `lean-rs-sys` | Raw `lean_*` declarations, inline mirrors of `lean.h`, the header check and link directives | Safe wrappers; public layout fields |
| `lean-rs` | Runtime and thread handles, object ownership, ABI conversion, typed handles for names, levels, expressions and declarations, loading and initializers, checked exports, callbacks, errors, the bridge to the host shims | Lean semantics; what a caller's exports mean |
| `lean-rs-host` | Imports, sessions, elaboration, kernel checks, declaration and module queries, bounded `MetaM`, and the Lean shim packages behind them | `unsafe` code, which the crate forbids; scheduling; process policy |
| `lean-rs-worker-protocol` | Framed parent and child messages, their owned types and the environment names both sides read | A Lean link |
| `lean-rs-worker-parent` | Spawning children, deadlines, restarts, cleanup, worker sessions and parent-side diagnostics | A Lean link; how many workers a caller runs or when it retries |
| `lean-rs-worker-child` | The Lean-linked child: its serial request loop, session pool, module cache and exit on parent death | Restart decisions, which the parent makes |

Dependencies run one way. `lean-toolchain` and `lean-rs-sys` build on `lean-rs-abi`; `lean-rs` on `lean-rs-sys` and
`lean-toolchain`; `lean-rs-host` on `lean-rs`; the protocol on `lean-toolchain`; the parent on the protocol; the child
on the host and the protocol. Only `lean-rs-sys` and the crates that build on it link `libleanshared`, so the protocol
and the parent build and run without Lean. The `profiling` member and the fuzz package are not published.

Put new code in the lowest crate that owns its inputs. When one Lake command answers the question, ask Lake. A crate
never names a caller.

The source-package cache in `lean-toolchain` is the one lock between processes. It holds a kernel file lock on a cache
entry while it writes that entry the first time, so two processes never write one entry at once. The kernel releases the
lock when its holder exits.

Rejected designs:

- **A safe-looking wrapper over all of `lean.h`.** It hands ownership, initialization and layout duties to callers. Add
  safe operations that discharge those duties, and keep raw access in `lean-rs-sys`.
- **Lean semantics in Rust.** A Rust model of `Expr`, universes or environments would compete with Lean's. Rust handles
  wrap Lean values; Lean computes what they mean.
- **Recycling Lean in one process.** Lean has no contract to finalize and start again that would void every old object,
  symbol, task and initializer. A new process is the reset.
- **A crate per internal module.** A crate needs its own caller and its own compatibility boundary.
- **An outside raw binding.** The opaque types, the digest window, the symbol list and the mirrors are the contract; a
  wrapper over a binding without them does not provide it.

## Invariants

Each invariant names the tests that check it. Paths start at `crates/` unless they name another top-level directory. A
test marked "run by name" is ignored in routine runs; `AGENTS.md` says how to run it.

### Toolchain window

- **One table.** `SUPPORTED_TOOLCHAINS` in `lean-rs-abi/src/supported.rs` is the only list of supported releases; it is
  in version order, and no digest or release appears twice. Checked by
  `window_is_non_empty_and_ordered_by_first_version`, `every_entry_lists_at_least_one_version`, `digests_are_distinct`,
  `versions_are_distinct_across_entries`, `digests_are_64_lowercase_hex` and `lookups_round_trip` in
  `lean-rs-abi/src/supported.rs`.
- **Digest gate.** A build accepts the active `lean.h` only when its digest is in the table, and an unknown release is
  unsupported even if it builds. Checked by `baked_toolchain_is_in_the_supported_window`,
  `resolved_version_aliases_into_the_matched_entry` and `unknown_versions_are_not_supported` in
  `lean-toolchain/tests/version_window.rs`; `synthetic_unknown_version_is_not_supported` in
  `lean-toolchain/src/fingerprint.rs`; `resolved_version_is_in_supported_window` and
  `discovered_version_matches_resolved_or_aliases_it` in `lean-rs-sys/tests/layout.rs`.
- **Every symbol on every release.** No supported entry lists a missing symbol, and the link step resolves each of
  `REQUIRED_SYMBOLS`. Checked by `required_symbols_are_present_across_the_window` in `lean-rs-abi/src/lib.rs` and
  `lean-toolchain/tests/version_window.rs`, and `all_required_symbols_resolve_at_link_time` in
  `lean-rs-sys/tests/linkage.rs`.
- **Explicit sysroot.** An explicit `LEAN_SYSROOT` wins over every other probe, and a build never falls back to another
  toolchain. Checked by `explicit_sysroot_wins_over_every_other_probe` in `lean-toolchain/tests/discover.rs` and
  `an_explicit_sysroot_never_falls_back_to_ambient_discovery` in `lean-toolchain/src/build_helpers.rs`.
- **One-line failure.** A missing Lean gives a diagnostic on one line. Checked by
  `missing_lean_diagnostic_displays_on_one_line` in `lean-toolchain/tests/discover.rs`.

### Raw FFI

- **Opaque objects.** `lean_object` is zero-sized, nothing outside `lean-rs-sys` can build one, and code outside the
  crate cannot name its layout type. Checked by `opaque_lean_object_is_zero_sized` in `lean-rs-sys/tests/layout.rs` and
  `opacity_invariants_are_enforced_by_the_type_system` in `lean-rs-sys/tests/compile_fail_surface.rs`.
- **Mirrors match the header.** The private header mirror matches `lean.h`. Checked by `header_repr_matches_lean_h` in
  `lean-rs-sys/src/repr.rs`.
- **Reference counts.** The inline helpers balance increments and decrements on one thread and on many, add `n` in
  place, and leave scalar pointers alone. Checked by `single_threaded_inc_dec_round_trips`,
  `multi_threaded_inc_decrements_negated_count`, `inc_ref_n_adds_in_place` and `scalar_pointers_are_left_untouched` in
  `lean-rs-sys/src/refcount.rs`.
- **Scalar arrays.** The element size ignores the array linearity marker. Checked by
  `sarray_elem_size_ignores_linearity_marker` in `lean-rs-sys/src/ctor.rs`.
- **Unsafe says why.** Each `unsafe fn` in `lean-rs-sys` has a `# Safety` section, each `unsafe` block has a
  `// SAFETY:` comment, and each public function outside `extern` blocks is `unsafe` unless it reads only static
  metadata. Checked by `every_unsafe_fn_has_safety_section`, `every_unsafe_block_has_safety_comment` and
  `no_safe_public_functions_in_public_surface` in `lean-rs-sys/tests/safety_grep.rs`.
- **Rust-only helpers under Miri.** The pure-Rust helpers in `lean-rs-sys` run under Miri with tree borrows. Checked by
  the Miri job in `.github/workflows/sanitizer.yml`.

### Runtime, objects and threads

- **One start.** `LeanRuntime::init` starts Lean once per process and returns the same anchor on later calls. Checked by
  `init_is_idempotent_across_repeated_calls` in `lean-rs/src/runtime/tests.rs`.
- **Attached threads.** A thread attaches with `LeanThreadGuard` before it touches Lean and detaches when the guard
  drops; nested guards balance. Checked by `worker_thread_can_attach_and_finalize` and
  `nested_attach_guards_balance_depth` in `lean-rs/src/runtime/tests.rs`.
- **Owned and borrowed.** `Obj` owns one reference: `Clone` adds one and `Drop` releases it. `ObjRef` borrows without
  touching the count, and a panic inside a clone scope leaks nothing. Checked by `clone_increments_heap_refcount`,
  `borrow_does_not_adjust_refcount` and `panic_inside_clone_scope_does_not_leak_refcount` in
  `lean-rs/src/runtime/obj.rs`.
- **Compile-time rules.** No public Lean-derived handle is `Send` or `Sync`; no handle outlives its runtime and no
  session outlives its capabilities; raw decoding, generic binding, native loading and trust each need `unsafe`; the C
  representation trait is sealed, and outside code cannot forge a worker contract token. Checked by
  `surface_invariants_are_enforced_by_the_type_system` in `lean-rs/tests/compile_fail_surface.rs` and
  `lean-rs-host/tests/compile_fail_surface.rs`, with one case per file in `lean-rs/tests/compile_fail/` and
  `lean-rs-host/tests/compile_fail/`.
- **Curated root.** The curated surface is reachable from the `lean_rs` crate root. Checked by
  `l1_curated_surface_is_reachable_from_crate_root` in `lean-rs/tests/curated_surface.rs`.

### ABI conversion

- **Round trips.** Each first-order type, nested containers included, converts to Lean and back unchanged, with its
  owned references. Checked by the round-trip tests in `lean-rs/src/abi/tests.rs`, and
  `nested_native_containers_preserve_values_and_owned_references` and
  `boxed_float_decoding_preserves_initialized_bit_patterns` in `lean-rs/tests/abi_native_roundtrips.rs`.
- **Wrong shapes are errors.** A constructor of the wrong shape, or one without scalar storage, is an error and never a
  panic, and taking fields keeps their ownership. Checked by `malformed_object_shape_errors_without_panicking` and
  `take_ctor_objects_preserves_field_ownership` in `lean-rs/src/abi/structure.rs`, and
  `scalar_decoders_reject_a_valid_constructor_without_scalar_storage` in `lean-rs/tests/abi_native_roundtrips.rs`.
- **Fuzzed decoding.** The `abi_decode` fuzz target decodes arbitrary bytes under AddressSanitizer without a finding.
  Checked by the "ABI fuzz smoke" step in `.github/workflows/release.yml`, on Linux.

### Loading and exports

- **Checked exports.** A typed export binds only when the manifest signature matches the requested argument and return
  shapes and the library holds the symbol. Checked by `checked_export_lookup_succeeds_with_matching_manifest_signature`,
  `checked_export_lookup_rejects_wrong_argument_shape`, `checked_export_lookup_rejects_wrong_return_shape`,
  `checked_export_lookup_reports_missing_signature_metadata` and
  `checked_export_lookup_reports_manifest_symbol_missing_from_dylib` in `lean-rs/src/module/tests.rs`.
- **Refused before Lean runs.** A missing, malformed or stale manifest, an unsupported schema or toolchain, and a
  missing library, dependency, symbol or initializer each fail with their own stable code before any native code runs.
  Checked by the `*_reports_stable_code` tests in `lean-rs/src/module/preflight.rs`.
- **Worker contracts.** A worker contract cannot change its attested protocol, an invalid contract never reaches the
  native initializer, and the contract table refuses empty or ambiguous symbols. Checked by
  `worker_contract_symbols_cannot_change_their_attested_protocol`,
  `invalid_worker_contracts_never_enter_the_native_initializer` and
  `worker_contract_table_rejects_ambiguous_or_empty_symbols` in `lean-rs/src/module/trusted.rs`.
- **Initializers.** A failed initializer stays failed for every later handle, and a library's finalizer survives a
  failed initializer, a failed dependency lookup and the drop of a worker bundle. Checked by
  `failed_initializer_stays_failed_across_handles` in `lean-rs/src/module/tests/initializer_failure.rs`, and
  `finalizer_survives_primary_initializer_failure`, `finalizer_survives_later_dependency_lookup_failure` and
  `finalizer_survives_successful_worker_bundle_drop` in `lean-rs/src/module/tests/partial_bundle.rs`.
- **Images stay mapped.** Reopening a pinned image adds no wrapper; a value that escapes a rejected worker keeps its
  image; a capability bundle keeps its transitive dependency after its opener returns. Checked by
  `reopening_pinned_image_does_not_retain_additional_wrappers` and
  `rejected_loaded_worker_keeps_image_for_escaping_value` in `lean-rs/src/module/library.rs`, and
  `public_capability_bundle_keeps_transitive_dependency_after_opener_returns` in
  `lean-rs-worker-child/tests/loader_regressions.rs`.
- **No loader variables.** The template's binary and its worker run with every dynamic-loader variable removed, and with
  `checked_native_loader_environment` the parent refuses to spawn a child whose environment sets one. Checked by
  `shipped_template_launches_with_clean_loader_environment` in `lean-rs-worker-child/tests/loader_regressions.rs` and
  `checked_worker_rejects_loader_override_before_spawn` in `lean-rs-worker-parent/src/supervisor.rs`.

### Callbacks and shims

- **Registered callbacks.** A registered callback runs through a typed Lean export, a scoped callback may borrow stack
  data, and a callback can stop a Lean loop. Checked by `registered_callback_runs_through_typed_lean_export`,
  `scoped_progress_callback_can_borrow_stack_context` and `callback_can_stop_lean_loop_cleanly` in
  `lean-rs/tests/callback_registry.rs`.
- **No unwind into Lean.** A panic in a Rust callback or progress sink stops at the trampoline and returns a status.
  Checked by `callback_panic_is_contained_at_registry_trampoline` in `lean-rs/tests/callback_registry.rs`,
  `rust_callback_panic_is_caught_before_returning_to_lean` in `lean-rs/tests/callback_trampoline.rs` and
  `progress_sink_panic_is_contained_by_callback_trampoline` in `lean-rs-host/tests/progress.rs`.
- **Checked dispatch.** A stale registration id or a wrong payload tag is refused before the payload is read. Checked by
  `stale_registration_ids_are_rejected_before_dispatch`, `mismatched_tags_are_rejected_before_payload_access` and
  `status_bytes_round_trip` in `lean-rs/src/callback.rs`, and `wrong_payload_returns_status_without_panic` in
  `lean-rs/tests/string_callback_abi.rs`.
- **One shim payload.** The host crate's interop shims are a byte-for-byte copy of the canonical package. Checked by
  `host_interop_shims_payload_is_a_verbatim_copy_of_the_canonical_package_payload` in
  `lean-rs-host/tests/interop_shims_parity.rs`.
- **The shims own their contract.** The required host-shim bindings are complete and generated from the shim sources,
  and the bundled manifest carries them. Checked by `required_contract_is_complete_and_source_owned` in
  `lean-toolchain/src/host_shim_abi.rs`, and `generated_signature_table_matches_packaged_shim_exports` and
  `bundled_shim_manifest_contains_generated_signatures` in `lean-rs-host/src/host/shim_bindings.rs`.

### Errors

- **Stable codes.** Each diagnostic code has its own stable id. Checked by `diagnostic_code_as_str_is_stable` in
  `lean-rs/src/error/tests.rs` and `all_codes_have_distinct_stable_ids` in `lean-rs-host/tests/diagnostics.rs`.
- **Bounded messages.** Host messages, Lean exception text and panic payloads stop at `LEAN_ERROR_MESSAGE_LIMIT`, 4 KiB.
  Checked by `host_constructor_bounds_oversize_message`, `lean_exception_constructor_bounds_oversize_message` and
  `catch_callback_panic_bounds_oversize_panic_payload` in `lean-rs/src/error/tests.rs`.

### Lake

- **Ask Lake.** Code reads no Lake file and builds no path in Lake's layout. The exceptions are the files the CI step
  lists, each with its reason: workflows and release scripts that put Lake's library directories on the Linux loader
  path, the qualification harness, fixtures and tests that pin a library or write a small workspace by hand, and sources
  that ship a committed `lake-manifest.json` without parsing it. Checked by the step "Ask Lake, never read its files" in
  `.github/workflows/ci.yml`.
- **Import roots.** `lake env` gives the search path. lean-rs keeps Lake's order, drops Lake's own library directory,
  and refuses an answer it cannot check. Checked by `lean_path_keeps_lakes_order_and_drops_lakes_own_directory` and
  `lean_path_refuses_an_answer_lean_toolchain_cannot_check` in `lean-toolchain/src/lake.rs`.
- **Shared libraries.** `lake query <lib>:shared` builds the library and prints its path. With `--no-build`, Lake exits
  3 when it would have to build, and a probe is ready only for a built library with the same manifest. Checked by
  `a_query_returns_the_one_file_lake_prints` in `lean-toolchain/src/lake.rs` and
  `a_probe_is_ready_only_for_built_libraries_and_the_same_manifest` in `lean-toolchain/src/build_helpers.rs`.
- **The caller may run Lake.** Each question comes as a command and a reader, so a caller that runs its own processes
  runs the command and hands back the output. Checked by `a_caller_runs_the_command_and_hands_back_its_output` in
  `lean-toolchain/src/lake.rs`.
- **Lake's errors pass through.** A missing library or target keeps Lake's message, failure detail is one bounded line,
  and a root without a lakefile runs no Lake. Checked by `lakes_messages_for_a_missing_library_name_it`,
  `failure_detail_is_one_bounded_line` and `a_root_without_a_lakefile_runs_no_lake` in `lean-toolchain/src/lake.rs`, and
  `lakes_report_of_a_missing_target_passes_through` in `lean-toolchain/src/build_helpers.rs`.
- **Versioned shim output.** With a generated-output key, every library Lake reports must lie below the key's directory
  and be a regular file with one hard link. When one lies elsewhere, the build has Lake read its lakefiles again once,
  and a probe reports it not ready. The interop library comes first among dependencies. Checked by
  `a_keyed_build_has_lake_read_its_lakefiles_again_once`, `keyed_libraries_must_not_be_aliased` and
  `the_interop_library_comes_first_among_dependencies` in `lean-toolchain/src/build_helpers.rs`.
- **Rerun triggers.** Cargo reruns a build script when a top-level file or a Lean source changes. Checked by
  `source_triggers_cover_top_level_files_and_lean_sources` in `lean-toolchain/src/build_helpers.rs`.
- **Manifests.** A manifest with an old schema, or with a header digest that differs from the toolchain's, is refused.
  Checked by `old_manifest_schema_is_rejected` and `fingerprint_rejects_mismatched_header_digest` in
  `lean-toolchain/src/manifest_validation.rs`.

### Process groups and source packages

- **Owned groups.** `OwnedCommandGroup` runs a command in its own process group and signals only that group. When it
  cannot observe the group, cleanup stays pending and sends no signal; after `ECHILD` it never signals again, even if
  the pid comes back. Checked by `echild_permanently_revokes_signaling_even_if_pid_could_be_reused`,
  `observation_failure_keeps_cleanup_pending_without_signaling` and
  `stopping_one_group_leaves_an_unrelated_private_group_running` in `lean-toolchain/src/process_group/tests.rs`.
- **Source-package cache.** Concurrent first writes of one cache entry take turns on the entry's file lock; the wait is
  bounded and stops on cancellation. Checked by `concurrent_first_materialization_serializes_same_entry` and
  `a_held_entry_lock_bounds_the_wait_and_cancellation_stops_it` in `lean-toolchain/src/source_package.rs`.

### Host services

- **Import lock.** One import runs at a time in a process. A nested import fails at once, a panic releases the lock, and
  a wait that times out after 30 s neither enters Lean nor poisons the next call. Checked by
  `import_lock_reentry_fails_without_waiting`, `import_lock_panic_releases_guard`,
  `import_lock_concurrent_call_waits_then_succeeds` and `import_lock_timeout_does_not_enter_or_poison_next_call` in
  `lean-rs/src/host_bridge/import_transaction.rs`.
- **No session imports under plain `cargo test`.** A libtest binary that `cargo test` runs cannot import a host session
  unless `LEAN_RS_ALLOW_CARGO_TEST_HOST_IMPORTS=1` is set; nextest and binaries that are not tests pass. Checked by
  `cargo_test_import_guard_blocks_same_process_libtest_harnesses` and
  `cargo_test_import_guard_allows_nextest_override_and_non_test_binaries` in `lean-rs-host/src/host/capabilities.rs`.
- **Bracketed queries.** A bracketed import query returns serialized declaration metadata, and its result type cannot
  carry a Lean handle. Checked by `bracketed_import_query_returns_serialized_declaration_metadata` and
  `bracketed_import_query_result_surface_cannot_carry_lean_handles` in `lean-rs-host/tests/host_services.rs`.
- **Kernel evidence.** The kernel accepts a small theorem with evidence, rejects a bad proof, and classifies a
  non-declaration or a pathological input instead of failing. Checked by `kernel_check_small_theorem_returns_evidence`,
  `kernel_check_rejects_bad_proof`, `kernel_check_unsupported_on_non_declaration` and
  `kernel_check_classifies_unavailable_or_rejected_on_pathological_input` in `lean-rs-host/tests/host_services.rs`.
- **Optional symbols.** A missing optional shim symbol gives `Unsupported`. Checked by
  `meta_missing_optional_symbol_returns_unsupported` in `lean-rs-host/tests/host_services.rs`.
- **Budgets.** Elaboration and `MetaM` default to 200,000 heartbeats and stop at 200,000,000; diagnostics default to 64
  KiB a call and stop at 1 MiB; running out of heartbeats gives a timeout status. Checked by
  `defaults_match_published_constants` and `heartbeat_setter_saturates_at_max` in
  `lean-rs-host/src/host/elaboration/options.rs` and `lean-rs-host/src/host/meta/options.rs`, and
  `diagnostic_byte_limit_truncates` and `meta_heartbeat_burn_yields_timeout_status` in
  `lean-rs-host/tests/host_services.rs`.
- **Bulk calls.** A bulk call is one dispatch and returns a slot per input, missing names included. It checks
  cancellation before the first item and between items, and reports ordered progress that never goes back within a
  phase. Checked by `declaration_type_bulk_returns_present_and_missing_slots_in_one_dispatch`,
  `query_declarations_bulk_errors_on_missing_name`,
  `declaration_bulk_pre_cancelled_token_returns_cancelled_without_ffi`,
  `declaration_bulk_observes_cancellation_between_items` and `elaborate_bulk_returns_per_source_results` in
  `lean-rs-host/tests/batching.rs`; `no_progress_bulk_path_remains_one_dispatch`,
  `query_declarations_bulk_reports_ordered_multi_phase_progress`, `progress_current_is_monotonic_within_each_phase` and
  `sink_triggered_cancellation_fires_at_next_progress_boundary` in `lean-rs-host/tests/progress.rs`;
  `pre_cancelled_session_call_returns_cancelled_without_ffi` and `bulk_query_observes_cancellation_from_another_thread`
  in `lean-rs-host/tests/cancellation.rs`.
- **Extension epoch.** The extension-registry epoch moves only when a module registers an extension. Checked by
  `the_extension_registry_epoch_moves_only_when_a_module_registers` in `lean-rs-host/tests/bundled_shims.rs`.
- **Command output.** `#check` and `#print axioms` output comes back as diagnostics within the message budget; an
  invalid command gives error diagnostics, and a missing import is reported as module queries report it. Checked by the
  `command_message_capture_*` tests in `lean-rs-host/tests/host_services.rs`.

### Module queries

- **Field budgets.** Query text fields clamp at 1 MiB a field and 1 MiB a batch; the defaults are 8 KiB and 64 KiB.
  Checked by `module_query_output_budget_normalization_clamps_struct_literals` in
  `lean-rs/src/host_bridge/module_requests.rs`.
- **Each selector returns what it names.** A batch returns proof context in one dispatch; a goal selector returns the
  goals at one tactic, a type selector one term's type, and references only name locations; diagnostics come without the
  info tree; missing imports and header parse errors come back with the result. Checked by the
  `session_process_module_query_*` tests in `lean-rs-host/tests/host_services.rs`.
- **Source coordinates.** Diagnostics keep the coordinates of the original source. Checked by
  `process_module_query_diagnostics_keep_original_source_coordinates` in `lean-rs-worker-child/tests/typed_session.rs`.
- **Total budget.** A batch keeps to its total budget without killing the worker, and an oversized reply is a request
  error, not a child death. Checked by `process_module_query_batch_obeys_total_budget_without_killing_worker` and
  `oversized_terminal_response_is_request_error_not_child_death` in `lean-rs-worker-child/tests/typed_session.rs`.
- **Incomplete imports.** A query whose import closure is incomplete skips body elaboration. Checked by
  `module_query_on_incomplete_closure_skips_body_elaboration` in `lean-rs-worker-child/tests/typed_session.rs`.
- **Snapshot cache.** The same file content reuses its snapshot; changed content rebuilds and matches a cold build;
  clearing, the time limit and the entry limit evict; snapshots are kept apart by session imports; a tiny cache still
  answers correctly. Checked by `process_module_query_batch_reuses_snapshot_for_same_file_content`,
  `process_module_query_batch_rebuilds_when_content_changes`,
  `process_module_query_batch_incremental_rebuild_matches_a_cold_build`,
  `process_module_query_batch_clear_evicts_snapshot`, `process_module_query_batch_ttl_evicts_idle_snapshot`,
  `process_module_query_batch_entry_limit_evicts_old_entries`,
  `process_module_query_batch_isolates_snapshots_by_session_imports` and
  `process_module_query_batch_tiny_cache_still_returns_correct_query` in `lean-rs-worker-child/tests/typed_session.rs`.
- **Proof state.** Locals render pretty by default and raw on request; the entry selector renders the goal before any
  tactic; an unresolved position returns boundary candidates and says when it cut them short. Checked by
  `proof_state_in_declaration_renders_locals_pretty_by_default_and_raw_on_opt_out`,
  `proof_state_entry_selector_renders_pristine_entry_goal`,
  `proof_state_unresolved_after_text_returns_boundary_candidates_through_worker` and
  `proof_state_boundary_candidates_report_truncation_through_worker` in `lean-rs-worker-child/tests/typed_session.rs`.
- **Module system.** A module-system header is handled, and a public section resolves to one declaration. Checked by
  `process_module_query_handles_module_system_header_through_worker` and
  `module_public_section_resolves_to_unique_declaration` in `lean-rs-worker-child/tests/typed_session.rs`.

### Worker queries

- **Declarations.** Search applies filters, order and limits; core queries keep missing slots and type caps; inspection
  tells theorems, definitions and missing names apart and bounds its output. Checked by
  `search_declarations_applies_filters_orders_and_limits`,
  `core_declaration_queries_preserve_missing_slots_and_type_caps`,
  `inspect_declaration_distinguishes_theorem_definition_and_missing`,
  `inspect_declaration_bounds_output_and_opts_in_to_proof_search` and
  `inspect_declaration_truncates_and_distinguishes_renderings` in `lean-rs-worker-child/tests/typed_session.rs`.
- **Full listing.** Listing a whole environment streams rows past the frame cap. Checked by
  `list_declarations_strings_streams_full_env_without_frame_cap` in `lean-rs-worker-child/tests/typed_session.rs`.
- **Verification.** A closed theorem passes and `sorry` fails; a missing name carries similar names; an ambiguous name
  returns candidates and does not restart the child; an unbuilt import reports that it needs a build; a batch returns
  rows in target order. Checked by `verify_declaration_accepts_closed_theorem_and_rejects_sorry`,
  `verify_declaration_not_found_carries_similar_declarations`, `verify_declaration_reports_ambiguous_with_candidates`,
  `verify_declaration_ambiguous_open_reference_does_not_restart_child`,
  `verify_declaration_reports_needs_build_for_unbuilt_import` and
  `declaration_verification_batch_returns_ordered_rows_through_worker` in `lean-rs-worker-child/tests/typed_session.rs`.
- **Degraded verdicts.** Near its RSS ceiling a child cannot tell a missing name from an elaboration that memory
  pressure cut short, so with a ceiling set, a non-positive verdict at or above it becomes `BudgetExceeded`. An accepted
  verdict never changes; a ceiling of 0 or a missing RSS sample changes nothing; a degraded batch marks every selector
  and keeps target order. Checked by the `taint_*` tests in `lean-rs-worker-child/src/child.rs`, and
  `degraded_query_batch_outcome_marks_every_selector_budget_exceeded` and
  `declaration_verification_batch_degraded_result_preserves_target_order` in `lean-rs-worker-parent/src/supervisor.rs`.
- **Proof attempts.** The reply's entry state carries the goals and locals; a good candidate closes the goal; a bad one
  returns diagnostics, never reports a false close, and leaves the session usable; the candidate list is capped, rows
  come back in order, the global budget marks the rest not attempted, and nothing writes a file. Checked by
  `attempt_proof_envelope_entry_state_carries_goals_and_locals`,
  `attempt_proof_successful_candidate_closes_simple_goal`,
  `attempt_proof_bad_candidate_returns_diagnostics_and_session_survives`,
  `attempt_proof_bad_candidate_on_unicode_signature_does_not_false_close`,
  `attempt_proof_candidate_list_is_capped_and_does_not_write_files`, `attempt_proof_ten_candidates_returns_ordered_rows`
  and `attempt_proof_global_output_budget_returns_partial_not_attempted_rows` in
  `lean-rs-worker-child/tests/typed_session.rs`.

### Worker protocol

- **Bounded frames.** The parent sets the frame cap and sends it to the child after the handshake. The cap defaults to 1
  MiB and clamps between 64 KiB and 256 MiB, and a frame over it is refused before it is written and before the reader
  allocates for it. Checked by `oversized_data_row_is_rejected_before_write` and
  `oversized_data_row_is_rejected_before_read_allocation` in `lean-rs-worker-protocol/src/protocol.rs`, and
  `max_frame_bytes_clamps_above_ceiling` and `max_frame_bytes_clamps_below_floor` in
  `lean-rs-worker-parent/src/supervisor.rs`.
- **Malformed frames.** A malformed payload is a protocol error. Checked by `malformed_frame_payload_is_protocol_error`
  in `lean-rs-worker-protocol/src/protocol.rs`.
- **Row order.** Rows carry a sequence number per stream and arrive in pipe order. Checked by
  `data_row_emitter_assigns_per_stream_sequences` in `lean-rs-worker-protocol/src/protocol.rs` and
  `data_rows_are_delivered_in_pipe_order_with_per_stream_sequences` in `lean-rs-worker-child/tests/protocol.rs`.
- **Host metadata.** Metadata without the Lean runtime version, or with the wrong compiled header, is refused; the
  expected ABI must be a subset of the child's, so extra compatible bindings pass. The parent checks the metadata before
  it exposes a session and again after each replacement. Checked by
  `missing_runtime_version_or_wrong_compiled_header_is_not_accepted`,
  `expected_abi_is_a_subset_and_extra_compatible_bindings_are_allowed` and
  `opt_in_request_and_actual_response_round_trip` in `lean-rs-worker-protocol/tests/host_runtime_metadata.rs`, and
  `host_metadata_is_checked_before_session_is_exposed`, `host_metadata_is_revalidated_after_recycle`,
  `host_metadata_accepts_compatible_extra_bindings`, `host_handle_requires_host_metadata_before_first_session`,
  `host_handle_revalidates_host_metadata_after_replacement` and
  `host_metadata_unobserved_protocol_failure_has_no_terminal_refusal` in
  `lean-rs-worker-parent/tests/worker_shutdown.rs`.

### Supervisor

A worker runs one child at a time. Each admitted restart starts a new generation, even when its child then fails to
start, so the generation equals the restart count; a refused restart starts none. The parent admits one request at a
time and tags each event its reader passes on with the generation and the request. A planned restart carries out the
restart policy or an explicit `cycle`; an abnormal one replaces a child that failed. The tests below run against fake
children in `lean-rs-worker-parent` and against real ones in `lean-rs-worker-child/tests/`.

- **One terminal outcome.** Each admitted request gets exactly one terminal outcome, whether it succeeds or its child
  crashes or the restart window refuses a replacement. A successful write to stdin is not a success; only a terminal
  success response is. Checked by `conformance_terminal_success_has_one_terminal_outcome`,
  `conformance_name_rows_complete_with_one_terminal_outcome`, `conformance_child_crash_terminalizes_in_flight_request`
  and `conformance_restart_limit_exhaustion_is_typed_terminal_outcome` in
  `lean-rs-worker-parent/tests/worker_shutdown.rs`.
- **Rows before success are tentative.** A sink sees rows as they arrive. If the child then exits, aborts or goes
  silent, the request fails, so the caller must not commit those rows. Checked by
  `conformance_name_rows_then_child_exit_fail_the_request`, `conformance_name_rows_then_child_abort_fail_the_request`
  and `conformance_name_rows_then_silence_time_out` in `lean-rs-worker-parent/tests/worker_shutdown.rs`.
- **No stale output.** Output from an old generation or an old request cannot complete the current one, a stale terminal
  frame neither completes nor resends the current command, and a success read after the deadline is refused. Checked by
  `conformance_stale_generation_output_is_protocol_failure` and `conformance_stale_request_output_is_protocol_failure`
  in `lean-rs-worker-parent/src/supervisor.rs`, and `stale_terminal_cannot_complete_or_redispatch_the_current_command`
  and `configured_deadline_rejects_dequeued_success_and_restarts` in
  `lean-rs-worker-parent/src/supervisor/terminal_frame_tests.rs`.
- **Deadlines.** One deadline, 30 s by default, covers writing the request and reading the reply, and polling never
  renews it. A timeout kills and reaps the child; the next request starts the replacement. Checked by
  `send_and_response_share_deadline_and_replacement_remains_usable`,
  `conformance_silent_worker_deadline_is_not_renewed_by_polling`,
  `conformance_timeout_kill_reap_restarts_next_generation` and
  `expired_deadline_leaves_the_replacement_to_the_next_request` in `lean-rs-worker-parent/tests/worker_shutdown.rs`, and
  `blocked_stdin_write_times_out_and_reaps_child` in `lean-rs-worker-parent/src/supervisor.rs`.
- **Cancellation.** The wait checks its cancellation token at least every 50 ms, even when the child sends nothing, and
  cancelling kills and reaps only the child the worker owns. Checked by
  `conformance_silent_worker_cancellation_reaps_only_owned_child` in `lean-rs-worker-parent/tests/worker_shutdown.rs`,
  `blocked_stdin_write_is_cancellable_and_reaps_child` in `lean-rs-worker-parent/src/supervisor.rs` and
  `parent_can_cancel_long_worker_request_at_progress_boundary` in `lean-rs-worker-child/tests/session_adapter.rs`.
- **Shutdown.** `shutdown` sends `Terminate`, waits 2 s by default, kills the child if it is still running, waits up to
  5 s for the reap, and reports which path it took. `Drop` takes the same steps and discards the report. Checked by
  `conformance_explicit_shutdown_gracefully_reaps_child`, `conformance_dropped_idle_worker_reaps_child` and
  `conformance_dropped_worker_escalates_kill_and_reaps_child` in `lean-rs-worker-parent/tests/worker_shutdown.rs`.
- **Pipes never hold up cleanup.** Stderr drains without blocking a verbose child and keeps a 64 KiB tail; no reader
  waits for a pipe that a grandchild holds open; the wait for stdout to close is bounded. Checked by
  `stderr_drain_does_not_block_a_verbose_child_and_keeps_bounded_tail`,
  `stderr_capture_does_not_wait_for_inherited_pipe_to_close`, `stdout_eof_wait_is_bounded_for_a_child_that_stays_alive`
  and `stopped_stdout_reader_joins_while_pipe_holder_remains_alive` in `lean-rs-worker-parent/src/supervisor.rs`.
- **Backpressure.** A slow progress sink fills the reader's bounded event buffer; the reader waits, counts each wait in
  `backpressure_waits`, and still delivers every event. Checked by
  `conformance_slow_progress_sink_backpressure_is_bounded_and_observable` in
  `lean-rs-worker-parent/tests/worker_shutdown.rs`.
- **Restart window.** Planned restarts never count against the window; abnormal ones do, 16 a minute by default. A
  refused replacement returns `RestartLimitExceeded` with the time until the oldest counted restart leaves the window;
  until then requests and `cycle` fail with that error, and then the next request starts a child. Checked by
  `planned_restarts_do_not_use_the_restart_window`, `crashed_child_is_replaced_and_counted` and
  `exhausted_worker_starts_again_after_the_window` in `lean-rs-worker-parent/tests/worker_shutdown.rs`, and
  `restart_intensity_policy_clamps_to_nonzero_window`, `restart_reasons_name_their_cause_and_kind` and
  `lifecycle_snapshot_exposes_restart_generation` in `lean-rs-worker-parent/src/supervisor.rs`.
- **Restart policy.** The request, import, idle and RSS limits each restart the child before the next request or import;
  a crash and a policy restart report different reasons; an explicit `cycle` records its reason; a stopped worker
  refuses requests until `cycle`; the next request replaces a dead child. Checked by
  `max_request_policy_restarts_before_next_request`, `max_import_policy_restarts_before_next_import`,
  `idle_restart_policy_restarts_before_next_request`, `rss_policy_restarts_or_records_unavailable_sample`,
  `child_crash_and_policy_restart_are_distinguishable`, `explicit_cycle_replaces_child_and_records_reason` and
  `next_request_replaces_a_dead_child` in `lean-rs-worker-child/tests/supervisor.rs`, and
  `stopped_worker_refuses_requests_until_cycle` and `terminal_refusal_survives_until_later_dispatch` in
  `lean-rs-worker-parent/tests/worker_shutdown.rs`.
- **Import residue.** The bytes that imports leave behind add up over a generation and reset on the next; only confirmed
  imports count, an unknown size stays unknown, and a single import larger than the whole budget still runs. Checked by
  `import_residue_accumulates_over_a_generation_and_resets_on_the_cycle` and
  `a_child_that_reports_no_region_bytes_is_counted_and_still_bounded` in
  `lean-rs-worker-parent/tests/worker_shutdown.rs`, and `residue_accumulates_only_over_confirmed_imports`,
  `header_import_accounting_is_cumulative_and_unknown_is_sticky` and `a_single_import_over_the_whole_budget_still_runs`
  in `lean-rs-worker-parent/src/supervisor.rs`.
- **Header budget.** Once the budget is spent, a warm header is still reused and a new one recycles the child and
  retries once; a failed header keeps its result and replaces the child before the next request; a second recycle fails
  instead of looping. Checked by `header_budget_reuses_warm_and_retries_distinct_once`,
  `failed_header_preserves_result_and_retires_next_request` and `header_recycle_is_retried_only_once` in
  `lean-rs-worker-parent/tests/worker_shutdown.rs`.
- **Worker sessions.** A missing session opens again in a new child; a real session miss recycles once and a warm reuse
  does not; a stale warm session keeps the import-count limit; a change of artifact epoch recycles all pooled state
  before the request body runs. Checked by `missing_session_is_opened_again_in_a_new_child`,
  `actual_session_miss_recycles_once_but_warm_reuse_does_not`, `stale_warm_session_keeps_import_count_backstop`,
  `failed_header_then_name_listing_reopens_session` and `artifact_epoch_change_recycles_all_pooled_state_before_body` in
  `lean-rs-worker-parent/tests/worker_shutdown.rs`.
- **Failed replacement.** If a replacement fails to start and cleanup cannot confirm that it exited, the worker keeps
  its handle, runs nothing on it, and never takes the old child's exit as proof. Checked by
  `failed_replacement_retains_unconfirmed_child_instead_of_reusing_old_exit`,
  `failed_replacement_releases_only_after_confirmed_cleanup_or_no_spawn` and
  `startup_failure_guard_reaps_owned_child_and_preserves_other_child` in `lean-rs-worker-parent/src/supervisor.rs`.
- **Stop receipts.** Take `request_checkpoint()` before an operation; `confirmed_request_stop_since` then names the
  latest attempted request, its generation, its child's pid and the confirmed exit. A receipt survives replacement and
  covers a partly written request; a newer attempt hides an older receipt; request and generation counters refuse to
  wrap. A receipt proves only that the request cannot go on in that child, not that it had no effects. Checked by
  `request_stop_receipt_matches_checkpoint_across_replacement` and `newer_request_shadows_older_stop_receipt` in
  `lean-rs-worker-parent/tests/worker_shutdown.rs`, and `partial_write_stop_receipt_survives_failed_replacement`,
  `newer_unreaped_request_cannot_reuse_older_stop_receipt` and `exhausted_request_identifiers_reject_before_dispatch` in
  `lean-rs-worker-parent/src/supervisor.rs`.
- **Process count.** The process tracker holds a count from before spawn until a confirmed exit. It reaps a dropped
  child without touching another, never settles one child with another's exit, and releases no count it does not own.
  Checked by `reaper_start_failure_prevents_launch_and_releases_no_unowned_capacity`,
  `failed_launch_holds_reservation_until_spawn_returns`, `final_client_loss_reaps_without_external_tracker_poll`,
  `exit_observation_never_settles_a_different_live_child` and `dropped_child_is_reaped_without_touching_another_child`
  in `lean-rs-worker-parent/src/process.rs`.
- **Startup.** A missing child binary or a failed handshake is a bootstrap diagnostic, `check` finds a missing child
  without spawning one, startup times out after 10 s by default, and a child that fails before the handshake reports its
  stderr. Checked by `missing_worker_child_is_a_bootstrap_error`,
  `bootstrap_report_distinguishes_missing_child_without_spawning` and `handshake_failure_is_a_bootstrap_diagnostic` in
  `lean-rs-worker-child/tests/host_handle.rs`, and `startup_timeout_is_typed` and
  `pre_handshake_child_failure_carries_stderr` in `lean-rs-worker-child/tests/supervisor.rs`.
- **Child limits.** The parent passes module-cache limits to the child as typed settings, and 0 turns a limit off rather
  than making it as strict as possible. Checked by `module_cache_limits_map_to_typed_child_policy_env` and
  `zero_disables_rather_than_becoming_the_strictest_setting` in `lean-rs-worker-parent/src/capability.rs`.

### Worker child

- **Parent death.** The child holds the read end of a pipe named by `LEAN_RS_WORKER_PARENT_LIFELINE` and exits with
  status 125 when the read returns end of file, even in the middle of a Lean call. An invalid lifeline stops startup
  before the body runs. Checked by `busy_child_exits_when_the_parent_closes_its_lifeline` and
  `invalid_lifelines_stop_startup_before_the_body` in `lean-rs-worker-child/src/parent_watchdog/tests.rs`, and
  `parent_death::busy_worker_exits_when_its_parent_is_killed` in `lean-rs-worker-child/tests/supervisor.rs`, which kills
  the parent with SIGKILL while the child runs a busy loop in Lean.
- **Control channel.** The child exits when stdin closes or a stdout write fails, without waiting for another request.
  Checked by `stdin_eof_exits_without_waiting_for_another_request` and
  `broken_stdout_write_exits_without_waiting_for_another_request` in `lean-rs-worker-child/tests/protocol.rs`.
- **Fatal exits stay in the child.** The parent starts each child with `LEAN_ABORT_ON_PANIC=1` unless the caller sets
  it, so a Lean panic aborts only the child, and the parent gets a typed failure with the exit status and the child's
  message. A fatal exit after some rows fails the request, and end of file before the rows finish is a protocol failure.
  Checked by `lean_internal_panic_kills_only_child`, `fatal_exit_after_partial_rows_is_reported_as_worker_failure` and
  `eof_before_rows_complete_is_reported_as_protocol_failure` in `lean-rs-worker-child/tests/protocol.rs`,
  `child_crash_is_typed_and_parent_survives` in `lean-rs-worker-child/tests/supervisor.rs`, and
  `a_child_abort_carries_the_exit_status_and_the_child_message` in `lean-rs-worker-parent/src/supervisor.rs`.
- **Session pool keys.** A pooled session is reused only for the same project root, mode, imports in the same order and
  import profile. Checked by `every_dimension_of_a_session_key_defeats_reuse`, `reordering_imports_defeats_reuse` and
  `the_module_query_cache_key_separates_projects_modes_and_import_profiles` in `lean-rs-worker-child/src/child.rs`, and
  `a_pooled_sibling_in_another_mode_never_answers_from_the_wrong_environment` in
  `lean-rs-worker-child/tests/session_reuse.rs`.
- **Session pool reuse.** Reopening an identical session or returning to a pooled import set imports nothing and keeps
  the snapshot cache; a mispredicted reuse restarts before the real import; a reused session carries no state from the
  last one; neither identical reopens nor alternation between pooled profiles grows the child. Checked by
  `reopening_an_identical_session_does_not_import_again`, `the_snapshot_cache_survives_a_reuse`,
  `switching_back_to_a_pooled_import_set_does_not_reimport`, `a_pool_with_room_serves_the_return_without_importing`,
  `an_alternation_that_fits_the_pool_never_evicts`, `a_mispredicted_reuse_restarts_before_the_actual_import`,
  `max_imports_does_not_restart_on_an_identical_reopen` and `a_reused_session_carries_no_state_from_the_previous_one` in
  `lean-rs-worker-child/tests/session_reuse.rs`; there too `repeated_identical_opens_do_not_grow_the_child` and
  `alternating_two_pooled_profiles_does_not_grow_the_child`, which measure RSS and run by name.
- **Session pool size.** A full pool evicts the least recently used session and clears its snapshots; capacity 1 gives
  the one-session child; the pool size is separate from the import restart limit, and an explicit child setting wins
  over the configured size. Checked by `a_full_pool_evicts_least_recently_used_and_re_imports`,
  `a_capacity_eviction_clears_the_snapshot_cache`, `capacity_one_restores_the_pre_pool_child`,
  `pool_capacity_is_independent_of_the_import_restart_bound` and
  `an_explicit_child_environment_outranks_the_configured_capacity` in `lean-rs-worker-child/tests/session_reuse.rs`.
- **Late extensions.** An extension registered after an environment was imported never reaches that pooled environment,
  which the pool drops; an import that registers nothing leaves the pool alone. Checked by
  `a_late_extension_registration_never_reaches_a_pooled_environment`,
  `a_late_extension_registration_evicts_the_pooled_environment` and
  `an_import_that_registers_nothing_leaves_the_pool_intact` in `lean-rs-worker-child/tests/session_reuse.rs`.
- **RSS guard.** The snapshot cache clears only when an RSS sample exists and is at or above the guard, and a guard of 0
  never clears it. Checked by `rss_guard_clears_only_at_or_above_an_available_sample`,
  `rss_guard_unavailable_sample_does_not_clear` and `rss_guard_zero_never_clears` in
  `lean-rs-worker-child/src/child.rs`.
- **One plugin per child.** The first import attempt fixes the child's Lean plugin, its library and initializer, and a
  later import that names another plugin, or none, is refused. A source request runs only when the import roots the
  child kept match the expected order. Checked by `first_import_attempt_fixes_plugin_for_the_worker_generation` and
  `source_grant_requires_validation_of_the_retained_import_order` in `lean-rs-worker-child/src/child.rs`.
- **Prepared shims.** `--lean-rs-prepare-host-artifacts` builds the shims in a new directory and prints their
  descriptor. A launch uses exactly that snapshot or refuses, refuses a descriptor of another protocol, loads only the
  manifest bytes it admitted, and never hands a refused manifest to the native loader. Checked by
  `prepare_writes_and_builds_the_shims_in_a_new_directory` in `lean-rs-worker-child/tests/host_artifacts.rs`, which
  builds the shims with Lake and runs by name; `launch_selects_the_exact_prepared_snapshot_or_refuses`,
  `launch_refuses_a_descriptor_of_another_protocol` and `refused_manifest_reads_never_reach_the_native_loader` in
  `lean-rs-worker-child/src/host_artifacts.rs`; `bootstrap_rejects_valid_authorization_for_another_manifest_snapshot` in
  `lean-rs-worker-child/src/child.rs`; `ordinary_worker_refuses_invalid_prepared_shims_before_handshake` in
  `lean-rs-worker-child/tests/session_adapter.rs`.
- **Capability metadata.** Runtime and capability metadata stay apart, doctor reports carry pass, warning and error
  diagnostics, and malformed or missing exports are typed errors. A session's import profile reaches the child's import.
  Checked by `runtime_and_capability_metadata_are_separate`, `doctor_reports_pass_warning_and_error_diagnostics`,
  `malformed_metadata_and_doctor_json_are_typed` and `missing_metadata_and_doctor_exports_are_typed_worker_errors` in
  `lean-rs-worker-child/tests/metadata_doctor.rs`, and `session_import_profile_reaches_child_import` in
  `lean-rs-worker-child/tests/host_handle.rs`.

### Packaging and API

- **Deliberate API.** The API of each published crate except `lean-rs-abi` matches its baseline in `docs/api-review/`,
  and a change updates the baseline in the same commit. Checked by the "Public-API diff vs committed baselines" step in
  `.github/workflows/ci.yml`, and by `scripts/prerelease.sh`.
- **Packages build without Lean.** Each crate's package holds the files it needs, and its docs build from the tarball
  with `DOCS_RS=1` and no Lean on `PATH`. Checked by `scripts/check_package_docsrs.py`.

## Known limits

These hold today, and no test can change them.

- **The attribute map.** Lean seeds each new environment's attribute map from a process-wide map that only grows, so a
  warm child accepts attribute syntax that only an earlier import registered, where a fresh `lean` would not. The kernel
  never reads that map, so proofs stay sound, but an elaboration can pass in a warm child and fail in an editor. Only
  `max_imports(1)`, or a pool of one with a `cycle` per import, closes this, at the cost of a full import per switch.
- **The macOS lifeline gap.** macOS has no atomic close-on-exec pipe call, so the standard library calls `pipe` and then
  sets close-on-exec. lean-rs serializes that step with its own spawns, but a process that another thread starts in the
  gap, such as a Lake build, inherits the write end, and the worker then outlives its parent until that process exits.
  On Linux the standard library creates the pipe close-on-exec in one call.
- **Descendants.** The lifeline ends only the worker child. A process the child starts keeps running until its own owner
  stops it.
- **Inherited descriptors.** A child inherits every descriptor its parent left open without close-on-exec, and lean-rs
  never closes a descriptor it did not open.
- **No reset in one process.** Dropping a session releases its references, not Lean's process-wide imports,
  initializers, extension registries or allocator state.
- **In-process controls are cooperative.** Heartbeat budgets, cancellation checks and output budgets bound Lean work
  that checks them. They cannot stop Lean at an arbitrary point or recover from an abort; a worker child can.
- **Heartbeats are not a clock.** A heartbeat budget bounds Lean's work, not elapsed time; the parent's deadline bounds
  time.
- **RSS is sampled.** RSS limits act on samples. They are policy, not proof that memory came back.
- **A kill does not undo effects.** A request that times out or is cancelled may have had effects in the child before it
  died.
- **Sanitizers.** ASan runs only `lean-toolchain`'s tests, and Miri only `lean-rs-sys`'s Rust-only helpers; neither
  instruments Lean itself.
