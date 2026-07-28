//! Downstream-shape proof for bundled host shims.

#![allow(clippy::expect_used, clippy::panic)]

use std::path::PathBuf;

use lean_rs::LeanRuntime;
use lean_rs_host::LeanHost;

fn fixture_lake_root() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace = manifest_dir
        .parent()
        .and_then(std::path::Path::parent)
        .expect("crates/<name>/ lives two directories beneath the workspace root");
    workspace.join("fixtures").join("lean")
}

#[test]
fn host_loads_with_no_consumer_shim_require() {
    let lake_root = fixture_lake_root();
    let manifest = std::fs::read_to_string(lake_root.join("lake-manifest.json")).expect("fixture manifest is readable");
    assert!(
        !manifest.contains("lean_rs_host_shims") && !manifest.contains("lean_rs_interop_shims"),
        "fixture manifest should not require lean-rs shim packages: {manifest}",
    );

    let runtime = LeanRuntime::init().expect("Lean runtime initialisation must succeed");
    let host = LeanHost::from_lake_project(runtime, &lake_root).expect("host opens cleanly");
    let caps = host
        .load_capabilities("lean_rs_fixture", "LeanRsFixture")
        .expect("capabilities load with bundled shims");
    let mut session = caps
        .session(&["LeanRsFixture.Handles", "LeanRsHostShims.Meta"], None, None)
        .expect("session imports consumer and bundled shim modules");

    let kind = session
        .declaration_kind("LeanRsFixture.Handles.nameAnonymous", None)
        .expect("declaration kind query succeeds");
    assert_eq!(kind, "definition");
}

/// Pins the primitive every staleness check in the host stack is built on.
///
/// `Environment.extensions` is sized once, at import, and the growth path is
/// `private`, so an environment that outlives a later `registerEnvExtension`
/// can never be repaired. The stamp is how the pool and the worker child
/// notice. It sums three public append-only registries
/// (`persistentEnvExtensionsRef`, `scopedEnvExtensionsRef`, and the builtin
/// attribute count), so it must be *stable* across an import that registers
/// nothing and must *move* across one that registers something.
///
/// If a toolchain bump privatises one of those registries or changes when
/// initializers run, this is where it shows up first, and the diagnosis is
/// one line: the shim in `LeanRsHostShims/Environment.lean` no longer
/// observes registration.
#[test]
fn the_extension_registry_epoch_moves_only_when_a_module_registers() {
    let runtime = LeanRuntime::init().expect("Lean runtime initialisation must succeed");
    let host = LeanHost::from_lake_project(runtime, fixture_lake_root()).expect("host opens cleanly");
    // Shims-only on purpose: loading the fixture dylib would run
    // `LeanRsFixtureLateExtension`'s initializer eagerly, after
    // `lean_io_mark_end_initialization`, where registration throws. The
    // registration under test has to happen inside `Lean.importModules`.
    let caps = host.load_shims_only().expect("shim-only capabilities load");

    let baseline = caps
        .session(&["LeanRsFixture.Handles"], None, None)
        .expect("baseline session imports cleanly");
    let before = baseline.extension_registry_epoch();
    assert_eq!(
        baseline
            .live_extension_registry_epoch()
            .expect("live stamp reads cleanly"),
        before,
        "a session's recorded stamp is the live one until something registers",
    );

    let inert = caps
        .session(&["LeanRsFixture.Meta"], None, None)
        .expect("inert session imports cleanly");
    assert_eq!(
        inert.extension_registry_epoch(),
        before,
        "importing modules with no `initialize` block must not move the stamp",
    );
    assert_eq!(
        baseline
            .live_extension_registry_epoch()
            .expect("live stamp reads cleanly"),
        before,
        "an inert import must leave every already-imported environment usable",
    );

    let registrar = caps
        .session(&["LeanRsFixtureLateExtension"], None, None)
        .expect("late-extension session imports cleanly");
    let after = registrar.extension_registry_epoch();
    assert!(
        after > before,
        "registering a scoped environment extension must move the stamp: {before} -> {after}",
    );
    assert_eq!(
        baseline
            .live_extension_registry_epoch()
            .expect("live stamp reads cleanly"),
        after,
        "the stamp is process-global: every live session sees the same value, \
         which is exactly what makes the earlier environments detectably stale",
    );
}
