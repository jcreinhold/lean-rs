# Profiling

These opt-in workloads measure Lean import retention, worker CPU cost, and baseline reports. Normal tests use bounded
`cargo nextest` execution; profiling is diagnostic work.

## Baseline reports

From the repository root:

```sh
export RUSTFLAGS="-C target-cpu=native -C force-frame-pointers=yes"
cargo run --profile profiling -p lean-rs-profiling --bin collect_baseline_quick
cargo run --profile profiling -p lean-rs-profiling --bin collect_baseline_full
```

Both collectors build the profiling binaries and run bounded memory and latency workloads. The full collector also
records samply profiles. Results go under the git-ignored `profiling_results/` directory:

| Capture | JSON | Markdown |
| --- | --- | --- |
| Quick | `baseline_data_quick.json` | `profiling-baseline-quick.md` |
| Full | `baseline_data_full.json` | `profiling-baseline-full.md` |

Regenerate Markdown from a saved JSON capture:

```sh
cargo run --profile profiling -p lean-rs-profiling --bin generate_report -- --quick
cargo run --profile profiling -p lean-rs-profiling --bin generate_report
```

## Memory workloads

Run `./profiling/scripts/profile_memory.sh <workload>` with one of:

| Workload | Question measured |
| --- | --- |
| `long-session-fresh` | Retention after fresh same-process imports. |
| `long-session-steady` | Query/elaboration work after one import. |
| `long-session-matrix` | Exported/server/private profiles, explicit full-private compatibility, and no-extension diagnostics. |
| `long-session-bracketed` | One closed metadata query without extensions, followed by `freeRegions`. |
| `long-session-derived` | Source-range, pretty-printing, proof-search, parser/elaborator, snapshot, and lazy discriminator work. |
| `worker-cycling` | Child replacement under a small import budget. |

`long-session` runs the same-process modes in separate child processes, preventing retained imports from one mode from
contaminating another. Full sessions load extensions and cannot safely free compacted regions. The bracketed workload
returns only owned serialized data before freeing regions. Lazy discriminator initialization is derived-index work over
already imported data, not lazy `.olean` loading or unloading.

The wrappers default to one Lean thread and a 1,572,864 KiB RSS guard. Worker recaptures can set every policy value
explicitly:

```sh
LEAN_RS_WORKER_MEMORY_IMPORTS=6 \
LEAN_RS_WORKER_MEMORY_MAX_IMPORTS=1 \
LEAN_RS_WORKER_MEMORY_MAX_RSS_KIB=1572864 \
./profiling/scripts/profile_memory.sh worker-cycling
```

Increase fresh-import counts only after reviewing the previous peak:

```sh
LEAN_RS_LONG_SESSION_IMPORTS=8 \
LEAN_RS_LONG_SESSION_CHECKPOINT_EVERY=1 \
LEAN_RS_LONG_SESSION_MAX_RSS_KIB=1572864 \
./profiling/scripts/profile_memory.sh long-session-fresh
```

These are diagnostic settings, not portable service bounds. The collectors try `max_imports=2` only if the one-import
candidate stays at or below 70% of the 1.5 GiB cap, then recommend the largest measured candidate within budget. A
worker policy should measure retained import bytes and keep an import-count backstop, rather than infer memory from
counts alone.

`LEAN_RS_LEAN_MAX_MEMORY_KIB` supplies an opt-in Lean fail-fast limit checked periodically. It cannot reclaim import
state and does not replace cycling.

## Read the report rows

| Raw row | Report section and interpretation |
| --- | --- |
| `import_stats=...` | **Lean Import Stats**: total, mapped, and non-mapped compacted-region bytes; `imported_bytes` aliases the total. |
| `bracketed_import_stats=...` | Import attribution plus `free_regions_ran=true` after the no-extension bracket. |
| `query_derived_work=...` | **Lean Derived Work**: which query phases initialized derived structures. |
| `admission=...` | **Import Admission**: cold attempts, admissions/refusals, import-like requests, observed concurrent opens, and RSS before/after. |
| `replacement=...` | **Worker Replacement**: attempts/outcomes and spawn, import, and total timing. |

Replacement is synchronous (`synchronous-no-overlap`); prewarming needs evidence that the total RSS budget admits
overlap.

Absolute RSS values depend on the machine; compare recaptures on the same machine.

## CPU stacks

```sh
cargo install samply
./profiling/scripts/profile_with_samply.sh long-session
./profiling/scripts/profile_with_samply.sh worker-cycling
```

Profiles are saved as `profiling_results/<workload>.json.gz`. The analyzer flags mostly unsymbolicated captures; open
the saved profile in Firefox Profiler to inspect native Lean frames.
