//! An initializer registers a native finalizer which calls an earlier manifest-only helper.
use super::native_image::{build_image, link_image, private_dir};
use super::{LeanLibrary, fixture_manifest, runtime};
use crate::{LeanDiagnosticCode, Obj, module::AuthorizedWorkerSnapshot};
use std::{fs, mem::ManuallyDrop};

const REGISTRAR: &str = r"
#include <lean/lean.h>
extern void COUNTER_SYMBOL(void *);

static lean_external_class *cls;

static void finalize(void *counter) { COUNTER_SYMBOL(counter); }
static void foreach(void *counter, b_lean_obj_arg visit) { (void)counter; (void)visit; }

LEAN_EXPORT lean_obj_res initialize_Registrar(uint8_t builtin) {
    if (!cls) {
        cls = lean_register_external_class(finalize, foreach);
    }
    return lean_io_result_mk_ok(lean_box(0));
}

LEAN_EXPORT lean_obj_res make_counter(void *counter) {
    return cls ? lean_alloc_external(cls, counter) : NULL;
}
";

#[derive(Clone, Copy)]
enum LoadCase {
    Success,
    MissingDependencyInitializer,
    FailingPrimary,
}

fn check_finalizer(case: LoadCase) {
    let (helper_symbol, primary_source) = match case {
        LoadCase::Success => ("successful_bundle_increment", ""),
        LoadCase::MissingDependencyInitializer => ("incomplete_bundle_increment", ""),
        LoadCase::FailingPrimary => (
            "failed_primary_increment",
            r#"initialize bad : Nat ← throw (IO.userError "primary refused")"#,
        ),
    };
    let dir = private_dir();
    fs::write(
        dir.join("helper.c"),
        format!("#include <lean/lean.h>\nLEAN_EXPORT void {helper_symbol}(void *p) {{ ++*(uint64_t *)p; }}\n"),
    )
    .expect("write helper");
    fs::write(
        dir.join("registrar.c"),
        REGISTRAR.replace("COUNTER_SYMBOL", helper_symbol),
    )
    .expect("write registrar");
    let helper = link_image(&dir, "helper", &["helper.c"]);
    let registrar = link_image(&dir, "registrar", &["registrar.c"]);
    let primary = build_image(&dir, "primary", &[("Primary", primary_source)]);
    let runtime = runtime();
    #[allow(unsafe_code, reason = "hold separate references to the two private fixture images")]
    // SAFETY: these exact images were built above with the linked runtime; neither has a
    // loader constructor. Their guards remain live until both SDK pins are verified.
    let (helper_guard, registrar_guard) = unsafe {
        (
            LeanLibrary::open_globally(runtime, &helper).expect("hold helper"),
            LeanLibrary::open(runtime, &registrar).expect("hold registrar"),
        )
    };
    // If a check fails after registration, leave these two independent references
    // alive through process exit. Only a verified SDK owner permits releasing them.
    let helper_guard = ManuallyDrop::new(helper_guard);
    let registrar_guard = ManuallyDrop::new(registrar_guard);
    assert!(!helper_guard.is_pinned_for_process());
    assert!(!registrar_guard.is_pinned_for_process());

    let path = dir.join("worker.json");
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&fixture_manifest(&primary, "probe", "Primary", &[])).expect("manifest");
    let mut dependencies = vec![
        serde_json::json!({"dylib_path": helper, "export_symbols_for_dependents": true}),
        serde_json::json!({"dylib_path": registrar, "initializer": {"package": "probe", "module": "Registrar"}}),
    ];
    if matches!(case, LoadCase::MissingDependencyInitializer) {
        dependencies.push(serde_json::json!({"dylib_path": primary, "initializer": {"package": "probe", "module": "MissingDependencyInit"}}));
    }
    *manifest.get_mut("dependencies").expect("fixture dependency list") = dependencies.into();
    let bytes = serde_json::to_vec(&manifest).expect("encode dependencies");
    #[allow(unsafe_code, reason = "test application owns the complete worker snapshot")]
    // SAFETY: the exclusive directory contains the exact paired images. The dependency
    // order supplies the callback's helper. No worker body exports are admitted.
    let authority = unsafe { AuthorizedWorkerSnapshot::attest(runtime, path.clone(), bytes.clone(), vec![], &dir) };
    let result = authority.load_for(runtime, &path, &bytes);
    match case {
        LoadCase::Success => drop(result.expect("successful worker load")),
        LoadCase::MissingDependencyInitializer => {
            let error = result.err().expect("missing dependency initializer");
            assert_eq!(error.code(), LeanDiagnosticCode::Linking);
            assert!(error.to_string().contains("MissingDependencyInit"), "{error}");
        }
        LoadCase::FailingPrimary => {
            let error = result.err().expect("primary initializer must fail");
            assert_eq!(error.code(), LeanDiagnosticCode::ModuleInit);
            assert!(error.to_string().contains("primary refused"), "{error}");
        }
    }

    let mut calls = 0u64;
    #[allow(
        unsafe_code,
        reason = "the controlled registrar exports this exact object constructor"
    )]
    // SAFETY: make_counter accepts the address of a live u64 and returns a fresh Lean external
    // object, or null if registration never occurred. Both native images are still guarded.
    let raw = unsafe {
        let make = registrar_guard
            .resolve_function_symbol("make_counter")
            .expect("fixture constructor");
        let make: unsafe extern "C" fn(*mut std::ffi::c_void) -> *mut lean_rs_sys::lean_object =
            std::mem::transmute(make);
        make((&raw mut calls).cast())
    };
    assert!(!raw.is_null(), "registrar must have installed its class");
    #[allow(unsafe_code, reason = "the fresh external object transfers one reference")]
    // SAFETY: raw is the non-null owned fixture object in this runtime. calls outlives its
    // finalizer, and both independent guards survive an assertion failure.
    let object = unsafe { Obj::from_owned_raw(runtime, raw) };
    let pins = (
        registrar_guard.is_pinned_for_process(),
        helper_guard.is_pinned_for_process(),
    );
    assert_eq!(pins, (true, true), "SDK must own both registrar and callback helper");
    drop(ManuallyDrop::into_inner(registrar_guard));
    drop(ManuallyDrop::into_inner(helper_guard));
    drop(object);
    assert_eq!(calls, 1, "finalizer calls the earlier explicit dependency once");
    fs::remove_dir_all(dir).expect("remove fixture files");
}

#[test]
fn finalizer_survives_primary_initializer_failure() {
    check_finalizer(LoadCase::FailingPrimary);
}

#[test]
fn finalizer_survives_later_dependency_lookup_failure() {
    check_finalizer(LoadCase::MissingDependencyInitializer);
}

#[test]
fn finalizer_survives_successful_worker_bundle_drop() {
    check_finalizer(LoadCase::Success);
}

#[test]
fn ordinary_snapshot_remains_scoped() {
    let dir = private_dir();
    let primary = build_image(&dir, "scoped", &[("Scoped", "")]);
    let path = dir.join("scoped.json");
    let bytes = fixture_manifest(&primary, "probe", "Scoped", &[]);
    #[allow(unsafe_code, reason = "load the test-owned empty module with no escaping values")]
    // SAFETY: this private image has an empty module body and only the selected
    // runtime's standard Init dependency. There are no callbacks or exports.
    let capability =
        unsafe { crate::LeanCapability::from_manifest_snapshot(runtime(), &path, &bytes) }.expect("ordinary snapshot");
    assert!(!capability.library().is_pinned_for_process());
    drop(capability);
    fs::remove_dir_all(dir).expect("remove fixture files");
}
