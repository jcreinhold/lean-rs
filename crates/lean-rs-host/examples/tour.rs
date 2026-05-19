//! End-to-end tour of the curated `lean_rs::*` surface — every stage
//! composed in one process.
//!
//! The four focused examples (`theorem_query`, `evaluate`,
//! `proof_check`, `meta_query`) each demonstrate one concern. This
//! tour shows how they compose: host open → capability load →
//! environment import → two elaborate calls → one `kernel_check` →
//! one bulk declaration query → one `Meta.whnf`. Read it after the
//! focused examples when you want to see how a real workload threads
//! them together.
//!
//! Doubles as the cross-call regression probe from prompt 22 (guards
//! against drift from the borrowed-string `LeanAbi` impl). Output is
//! one `name=<stage> elapsed_us=<u64>` line per stage, suitable for
//! `grep`/`awk` — see `docs/performance/interventions.md` for the
//! recorded numbers.
//!
//! Why an example rather than a Criterion bench: each stage runs once
//! per process so warm-vs-cold distinctions stay visible.
//! `benches/session.rs` already covers the steady-state inner loop.

#![allow(clippy::expect_used, clippy::panic, clippy::print_stdout)]

use std::path::PathBuf;
use std::time::Instant;

use lean_rs::LeanRuntime;
use lean_rs_host::meta::{LeanMetaOptions, LeanMetaResponse, whnf};
use lean_rs_host::{LeanElabOptions, LeanHost, LeanKernelOutcome};

fn fixture_lake_root() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace = manifest_dir
        .parent()
        .and_then(std::path::Path::parent)
        .expect("crates/<name>/ lives two directories beneath the workspace root");
    workspace.join("fixtures").join("lean")
}

fn report(name: &str, elapsed_us: u128) {
    println!("name={name} elapsed_us={elapsed_us}");
}

fn main() {
    let lake_root = fixture_lake_root();
    assert!(
        lake_root.join(".lake").join("build").exists(),
        "fixture build not found at {} — run `cd fixtures/lean && lake build`",
        lake_root.display(),
    );

    // Runtime init is amortised across every prior invocation of this
    // binary (re-exec only); we still account for it so the binary is
    // self-contained.
    let runtime = LeanRuntime::init().expect("Lean runtime initialises");

    // Stage 1: open the Lake project as a `LeanHost`. Resolves the
    // toolchain prefix, fingerprints the .olean search path, and seeds
    // the project handle that `load_capabilities` consumes.
    let t = Instant::now();
    let host = LeanHost::from_lake_project(runtime, &lake_root).expect("host opens");
    report("host_open", t.elapsed().as_micros());

    // Stage 2: dlopen the fixture capability dylib and resolve the
    // session-fixed symbols. Per-call dispatch cost in later stages
    // comes from cached addresses set here.
    let t = Instant::now();
    let caps = host
        .load_capabilities("lean_rs_fixture", "LeanRsFixture")
        .expect("capabilities load");
    report("load_caps", t.elapsed().as_micros());

    // Stage 3: build a `LeanSession` over the imported environment.
    // Importing `Handles` and `Meta` together gives the elaborate /
    // kernel_check / run_meta calls below the prelude + fixture
    // declarations the bulk query exercises.
    let t = Instant::now();
    let mut session = caps
        .session(&["LeanRsFixture.Handles", "LeanRsHostShims.Meta"], None)
        .expect("session imports");
    report("session_import", t.elapsed().as_micros());

    let elab_opts = LeanElabOptions::new();

    // Stage 4 & 5: two `elaborate` calls. The repeat exists to surface
    // warm-vs-second-call drift inside one session.
    let t = Instant::now();
    let outcome = session
        .elaborate("(1 + 2 : Nat)", None, &elab_opts, None)
        .expect("host stack");
    drop(outcome.expect("(1 + 2 : Nat) elaborates"));
    report("elaborate_1", t.elapsed().as_micros());

    let t = Instant::now();
    let outcome = session
        .elaborate("(1 * 7 : Nat)", None, &elab_opts, None)
        .expect("host stack");
    drop(outcome.expect("(1 * 7 : Nat) elaborates"));
    report("elaborate_2", t.elapsed().as_micros());

    // Stage 6: one `kernel_check`. The declaration name must be unique
    // within this session — repeated invocations of this binary each get
    // a fresh process and a fresh environment.
    let t = Instant::now();
    let outcome = session
        .kernel_check(
            "theorem lean_rs_session_workflow_rfl : 1 + 1 = 2 := rfl",
            &elab_opts,
            None,
        )
        .expect("host stack");
    match outcome {
        LeanKernelOutcome::Checked(evidence) => drop(evidence),
        LeanKernelOutcome::Unavailable(failure)
        | LeanKernelOutcome::Rejected(failure)
        | LeanKernelOutcome::Unsupported(failure) => panic!("kernel_check did not check: {failure:?}"),
        _ => panic!("kernel_check returned an unexpected non-exhaustive variant"),
    }
    report("kernel_check_1", t.elapsed().as_micros());

    // Stage 7: bulk declaration query for three prelude names. Exercises
    // `query_declarations_bulk` plus the `make_name` per-element path
    // that intervention A (borrowed `&str` `LeanAbi`) optimised.
    let t = Instant::now();
    let decls = session
        .query_declarations_bulk(
            &[
                "LeanRsFixture.Handles.nameAnonymous",
                "LeanRsFixture.Handles.nameMkStr",
                "LeanRsFixture.Handles.exprConstNat",
            ],
            None,
        )
        .expect("bulk query");
    assert_eq!(decls.len(), 3, "bulk query returns one declaration per name");
    drop(decls);
    report("query_bulk_3", t.elapsed().as_micros());

    // Stage 8: one Meta.whnf invocation on `Nat.zero`'s type. Exercises
    // the `run_meta` shim through the `whnf` service descriptor.
    let expr = session
        .declaration_type("Nat.zero", None)
        .expect("type query")
        .expect("Nat.zero has a type");
    let meta_opts = LeanMetaOptions::new();
    let t = Instant::now();
    let outcome = session.run_meta(&whnf(), expr, &meta_opts, None).expect("host stack");
    match outcome {
        LeanMetaResponse::Ok(payload) => drop(payload),
        LeanMetaResponse::Failed(failure)
        | LeanMetaResponse::TimeoutOrHeartbeat(failure)
        | LeanMetaResponse::Unsupported(failure) => {
            panic!("Meta.whnf on Nat.zero's type expected Ok, got non-Ok: {failure:?}")
        }
        _ => panic!("Meta.whnf returned an unexpected non-exhaustive variant"),
    }
    report("meta_whnf", t.elapsed().as_micros());
}
