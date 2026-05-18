//! `cargo test -p lean-rs --lib module::tests`
//!
//! Tests run in the same process as `runtime::tests` / `abi::tests` /
//! `error::tests`, sharing the [`LeanRuntime::init`] cell. The fixture
//! dylib is located statically via `CARGO_MANIFEST_DIR`; on a fresh
//! clone where the fixture has never been built, the path-not-found
//! diagnostic from [`HostStage::Load`] names the file and the recovery
//! command (`cd fixtures/lean && lake build`).

#![allow(clippy::expect_used, clippy::panic)]

use std::path::PathBuf;

use super::initializer::InitializerName;
use super::{LeanLibrary, LeanModule};
use crate::error::{HostStage, LeanError};
use crate::runtime::LeanRuntime;

/// Resolve the fixture dylib path from the crate's manifest directory.
/// Probes both Lake naming conventions (Lean ≤ 4.26 vs ≥ 4.27) so the
/// tests work across the supported window.
fn fixture_dylib_path() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace = manifest_dir
        .parent()
        .and_then(std::path::Path::parent)
        .expect("crates/<name>/ lives two directories beneath the workspace root");
    let dylib_extension = if cfg!(target_os = "macos") { "dylib" } else { "so" };
    let lib_dir = workspace
        .join("fixtures")
        .join("lean")
        .join(".lake")
        .join("build")
        .join("lib");
    let new_style = lib_dir.join(format!("liblean__rs__fixture_LeanRsFixture.{dylib_extension}"));
    let old_style = lib_dir.join(format!("libLeanRsFixture.{dylib_extension}"));
    if old_style.is_file() && !new_style.is_file() {
        old_style
    } else {
        new_style
    }
}

fn runtime() -> &'static LeanRuntime {
    LeanRuntime::init().expect("Lean runtime initialisation must succeed")
}

fn open_fixture(runtime: &LeanRuntime) -> LeanLibrary<'_> {
    let path = fixture_dylib_path();
    assert!(
        path.exists(),
        "fixture dylib not found at {} — run `cd fixtures/lean && lake build`",
        path.display(),
    );
    LeanLibrary::open(runtime, &path).expect("fixture dylib opens cleanly")
}

#[test]
fn open_and_initialize_root_module_succeeds() {
    let runtime = runtime();
    let library = open_fixture(runtime);
    let module = library
        .initialize_module("lean_rs_fixture", "LeanRsFixture")
        .expect("root module initializer succeeds");
    assert_eq!(module.module_name(), "lean_rs_fixture::LeanRsFixture");
}

#[test]
fn initialize_module_is_idempotent() {
    let runtime = runtime();
    let library = open_fixture(runtime);
    let first = library
        .initialize_module("lean_rs_fixture", "LeanRsFixture.Scalars")
        .expect("first initializer call succeeds");
    let second = library
        .initialize_module("lean_rs_fixture", "LeanRsFixture.Scalars")
        .expect("second initializer call succeeds (Lean _G_initialized short-circuit)");
    assert_eq!(first.module_name(), second.module_name());
}

#[test]
fn missing_symbol_is_link_error() {
    let runtime = runtime();
    let library = open_fixture(runtime);
    let err = library
        .initialize_module("lean_rs_fixture", "NoSuchModule")
        .expect_err("missing initializer must surface a link error");
    match err {
        LeanError::Host(failure) => {
            assert_eq!(failure.stage(), HostStage::Link);
            let message = failure.message();
            // The diagnostic enumerates both candidates (modern + legacy)
            // so the operator can see which symbol shapes the loader
            // looked for. Pin only the modern form to avoid coupling
            // tests to the exact phrasing of the error.
            assert!(
                message.contains("initialize_lean__rs__fixture_NoSuchModule"),
                "diagnostic must name the modern initializer symbol, got: {message:?}",
            );
        }
        LeanError::LeanException(exc) => panic!("expected Host(Link) failure, got LeanException {exc:?}"),
    }
}

#[test]
fn missing_library_is_load_error() {
    let runtime = runtime();
    let err = LeanLibrary::open(runtime, "/does/not/exist/liblean__rs__fixture_missing.dylib")
        .expect_err("opening a nonexistent path must fail");
    match err {
        LeanError::Host(failure) => {
            assert_eq!(failure.stage(), HostStage::Load);
            assert!(
                failure.message().contains("liblean__rs__fixture_missing"),
                "diagnostic must name the requested path, got: {:?}",
                failure.message(),
            );
        }
        LeanError::LeanException(exc) => panic!("expected Host(Load) failure, got LeanException {exc:?}"),
    }
}

#[test]
fn invalid_module_name_is_link_error() {
    let runtime = runtime();
    let library = open_fixture(runtime);
    let err = library
        .initialize_module("lean_rs_fixture", "Has..Empty.Component")
        .expect_err("empty module component must fail validation");
    match err {
        LeanError::Host(failure) => {
            assert_eq!(failure.stage(), HostStage::Link);
            assert!(
                failure.message().contains("empty component"),
                "diagnostic must mention the empty component, got: {:?}",
                failure.message(),
            );
        }
        LeanError::LeanException(exc) => panic!("expected Host(Link) failure, got LeanException {exc:?}"),
    }
}

#[test]
fn invalid_package_name_is_link_error() {
    let runtime = runtime();
    let library = open_fixture(runtime);
    let err = library
        .initialize_module("9bad-package", "LeanRsFixture")
        .expect_err("non-identifier package must fail validation");
    match err {
        LeanError::Host(failure) => assert_eq!(failure.stage(), HostStage::Link),
        LeanError::LeanException(exc) => panic!("expected Host(Link) failure, got LeanException {exc:?}"),
    }
}

#[test]
fn symbol_table_walk_classifies_functions_and_globals() {
    let runtime = runtime();
    let library = open_fixture(runtime);
    let globals = library.globals();
    assert!(
        globals.contains("lean_rs_fixture_option_nat_none"),
        "Lean nullary-constant `optionNatNone` must be classified as a data-section global; \
         globals contained: {globals:?}",
    );
    assert!(
        !globals.contains("lean_rs_fixture_string_identity"),
        "the function export `lean_rs_fixture_string_identity` must NOT be in the globals set; \
         globals contained: {globals:?}",
    );
    assert!(
        !globals.contains("lean_rs_fixture_u8_identity"),
        "scalar identity exports are functions, not globals; globals contained: {globals:?}",
    );
}

#[test]
fn mangling_matches_fixture_symbols() {
    let root =
        InitializerName::from_lake_names("lean_rs_fixture", "LeanRsFixture").expect("root module name validates");
    assert_eq!(root.symbol_bytes(), b"initialize_lean__rs__fixture_LeanRsFixture\0",);

    let scalars = InitializerName::from_lake_names("lean_rs_fixture", "LeanRsFixture.Scalars")
        .expect("Scalars module name validates");
    assert_eq!(
        scalars.symbol_bytes(),
        b"initialize_lean__rs__fixture_LeanRsFixture_Scalars\0",
    );
    assert_eq!(scalars.display(), "lean_rs_fixture::LeanRsFixture.Scalars");
}

/// `LeanLibrary<'_>` and `LeanModule<'_, '_>` must be `!Send` and
/// `!Sync`. Uses the canonical `AmbiguousIfSend` / `AmbiguousIfSync`
/// trick from `runtime::obj::tests::not_send_not_sync`: the
/// `_compile_time_check` functions fail to type-check (E0283) if either
/// auto-trait is ever implemented.
#[allow(dead_code, reason = "items are consumed only via trait selection at compile time")]
mod not_send_not_sync {
    use super::{LeanLibrary, LeanModule};

    trait AmbiguousIfSend<A> {
        fn check() {}
    }
    impl<T: ?Sized> AmbiguousIfSend<()> for T {}
    struct Invalid;
    impl<T: ?Sized + Send> AmbiguousIfSend<Invalid> for T {}

    trait AmbiguousIfSync<A> {
        fn check() {}
    }
    impl<T: ?Sized> AmbiguousIfSync<()> for T {}
    struct InvalidSync;
    impl<T: ?Sized + Sync> AmbiguousIfSync<InvalidSync> for T {}

    fn _library_is_not_send() {
        <LeanLibrary<'static> as AmbiguousIfSend<_>>::check();
    }
    fn _library_is_not_sync() {
        <LeanLibrary<'static> as AmbiguousIfSync<_>>::check();
    }
    fn _module_is_not_send() {
        <LeanModule<'static, 'static> as AmbiguousIfSend<_>>::check();
    }
    fn _module_is_not_sync() {
        <LeanModule<'static, 'static> as AmbiguousIfSync<_>>::check();
    }
}
