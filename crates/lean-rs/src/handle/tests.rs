//! Round-trip tests for the four semantic handles.
//!
//! Each handle is exercised through the prompt-12
//! [`crate::module::LeanModule::exported`] dispatch path against the
//! `LeanRsFixture.Handles` capability. Construction and inspection happen
//! exclusively in Lean code; the Rust side only carries the handle.

#![allow(clippy::expect_used, clippy::panic)]

use std::path::PathBuf;

use crate::module::LeanLibrary;
use crate::runtime::LeanRuntime;
use crate::{LeanDeclaration, LeanExpr, LeanLevel, LeanName};

// -- fixture setup -------------------------------------------------------
// Mirrors the per-test-module pattern in `abi/tests.rs` and
// `module/tests.rs`: each test opens its own `LeanLibrary` against the
// process-wide runtime singleton; dlopen + symbol-table walks are backed
// by the OS page cache for the second-and-later open of the same file.

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

fn fixture_library() -> LeanLibrary<'static> {
    let path = fixture_dylib_path();
    assert!(
        path.exists(),
        "fixture dylib not found at {} — run `cd fixtures/lean && lake build`",
        path.display(),
    );
    LeanLibrary::open(runtime(), &path).expect("fixture dylib opens cleanly")
}

// -- LeanName ------------------------------------------------------------

#[test]
fn name_round_trips_through_lean_authored_display() {
    let library = fixture_library();
    let module = library
        .initialize_module("lean_rs_fixture", "LeanRsFixture")
        .expect("init root module");

    let mk_anon = module
        .exported::<((),), LeanName<'_>>("lean_rs_fixture_name_anonymous")
        .expect("lookup name_anonymous");
    let mk_str = module
        .exported::<(LeanName<'_>, String), LeanName<'_>>("lean_rs_fixture_name_mk_str")
        .expect("lookup name_mk_str");
    let to_str = module
        .exported::<(LeanName<'_>,), String>("lean_rs_fixture_name_to_string")
        .expect("lookup name_to_string");

    let root = mk_anon.call(()).expect("call name_anonymous");
    let child = mk_str.call(root, "Demo".to_owned()).expect("call name_mk_str");
    let rendered = to_str.call(child).expect("call name_to_string");

    assert!(
        rendered.contains("Demo"),
        "Lean-rendered name must contain the leaf component, got {rendered:?}",
    );
}

#[test]
fn name_equality_via_fixture_export() {
    let library = fixture_library();
    let module = library
        .initialize_module("lean_rs_fixture", "LeanRsFixture")
        .expect("init root module");

    let mk_anon = module
        .exported::<((),), LeanName<'_>>("lean_rs_fixture_name_anonymous")
        .expect("lookup name_anonymous");
    let mk_str = module
        .exported::<(LeanName<'_>, String), LeanName<'_>>("lean_rs_fixture_name_mk_str")
        .expect("lookup name_mk_str");
    let beq = module
        .exported::<(LeanName<'_>, LeanName<'_>), bool>("lean_rs_fixture_name_beq")
        .expect("lookup name_beq");

    let a = mk_str
        .call(mk_anon.call(()).expect("anon a"), "Foo".to_owned())
        .expect("mk a");
    let b = mk_str
        .call(mk_anon.call(()).expect("anon b"), "Foo".to_owned())
        .expect("mk b");
    let c = mk_str
        .call(mk_anon.call(()).expect("anon c"), "Bar".to_owned())
        .expect("mk c");

    assert!(beq.call(a.clone(), b).expect("beq equal"));
    assert!(!beq.call(a, c).expect("beq differ"));
}

// -- LeanLevel -----------------------------------------------------------

#[test]
fn level_round_trips_succ_zero() {
    let library = fixture_library();
    let module = library
        .initialize_module("lean_rs_fixture", "LeanRsFixture")
        .expect("init root module");

    let zero = module
        .exported::<((),), LeanLevel<'_>>("lean_rs_fixture_level_zero")
        .expect("lookup level_zero");
    let succ = module
        .exported::<(LeanLevel<'_>,), LeanLevel<'_>>("lean_rs_fixture_level_succ")
        .expect("lookup level_succ");
    let to_str = module
        .exported::<(LeanLevel<'_>,), String>("lean_rs_fixture_level_to_string")
        .expect("lookup level_to_string");

    let one = succ.call(zero.call(()).expect("zero")).expect("succ zero");
    let rendered = to_str.call(one).expect("level_to_string");

    // Lean prints `succ zero` as `"1"`; assert a non-empty render and a
    // digit somewhere so the test is robust against pretty-printer
    // formatting tweaks across toolchain updates.
    assert!(
        rendered.chars().any(|c| c.is_ascii_digit()),
        "Lean-rendered succ-zero must contain a digit, got {rendered:?}",
    );
}

#[test]
fn level_equality_via_fixture_export() {
    let library = fixture_library();
    let module = library
        .initialize_module("lean_rs_fixture", "LeanRsFixture")
        .expect("init root module");

    let zero = module
        .exported::<((),), LeanLevel<'_>>("lean_rs_fixture_level_zero")
        .expect("lookup level_zero");
    let succ = module
        .exported::<(LeanLevel<'_>,), LeanLevel<'_>>("lean_rs_fixture_level_succ")
        .expect("lookup level_succ");
    let beq = module
        .exported::<(LeanLevel<'_>, LeanLevel<'_>), bool>("lean_rs_fixture_level_beq")
        .expect("lookup level_beq");

    let a = succ.call(zero.call(()).expect("zero a")).expect("succ a");
    let b = succ.call(zero.call(()).expect("zero b")).expect("succ b");
    let c = zero.call(()).expect("zero c");

    assert!(beq.call(a.clone(), b).expect("beq equal"));
    assert!(!beq.call(a, c).expect("beq differ"));
}

// -- LeanExpr ------------------------------------------------------------

#[test]
fn expr_const_nat_round_trips() {
    let library = fixture_library();
    let module = library
        .initialize_module("lean_rs_fixture", "LeanRsFixture")
        .expect("init root module");

    let const_nat = module
        .exported::<((),), LeanExpr<'_>>("lean_rs_fixture_expr_const_nat")
        .expect("lookup expr_const_nat");
    let to_str = module
        .exported::<(LeanExpr<'_>,), String>("lean_rs_fixture_expr_to_string")
        .expect("lookup expr_to_string");

    let rendered = to_str
        .call(const_nat.call(()).expect("const Nat"))
        .expect("expr_to_string");

    assert!(
        rendered.contains("Nat"),
        "Lean-rendered `.const ``Nat []` must contain \"Nat\", got {rendered:?}",
    );
}

#[test]
fn expr_app_round_trips() {
    let library = fixture_library();
    let module = library
        .initialize_module("lean_rs_fixture", "LeanRsFixture")
        .expect("init root module");

    let const_nat = module
        .exported::<((),), LeanExpr<'_>>("lean_rs_fixture_expr_const_nat")
        .expect("lookup expr_const_nat");
    let app = module
        .exported::<(LeanExpr<'_>, LeanExpr<'_>), LeanExpr<'_>>("lean_rs_fixture_expr_app")
        .expect("lookup expr_app");
    let bvar = module
        .exported::<(u64,), LeanExpr<'_>>("lean_rs_fixture_expr_bvar")
        .expect("lookup expr_bvar");

    let f = const_nat.call(()).expect("f");
    let x = bvar.call(0).expect("x");
    let applied = app.call(f, x).expect("expr_app");
    // Round-tripped through Lean: passing it back through expr_app
    // should not error and should produce another distinct expression.
    let again = app
        .call(applied.clone(), bvar.call(1).expect("y"))
        .expect("expr_app second time");
    drop(again);
}

#[test]
fn expr_equality_via_fixture_export() {
    let library = fixture_library();
    let module = library
        .initialize_module("lean_rs_fixture", "LeanRsFixture")
        .expect("init root module");

    let const_nat = module
        .exported::<((),), LeanExpr<'_>>("lean_rs_fixture_expr_const_nat")
        .expect("lookup expr_const_nat");
    let bvar = module
        .exported::<(u64,), LeanExpr<'_>>("lean_rs_fixture_expr_bvar")
        .expect("lookup expr_bvar");
    let beq = module
        .exported::<(LeanExpr<'_>, LeanExpr<'_>), bool>("lean_rs_fixture_expr_beq")
        .expect("lookup expr_beq");

    let a = const_nat.call(()).expect("a");
    let b = const_nat.call(()).expect("b");
    let c = bvar.call(0).expect("c");

    assert!(beq.call(a.clone(), b).expect("beq equal"));
    assert!(!beq.call(a, c).expect("beq differ"));
}

// -- LeanDeclaration -----------------------------------------------------

#[test]
fn declaration_demo_axiom_round_trips() {
    let library = fixture_library();
    let module = library
        .initialize_module("lean_rs_fixture", "LeanRsFixture")
        .expect("init root module");

    let mk_anon = module
        .exported::<((),), LeanName<'_>>("lean_rs_fixture_name_anonymous")
        .expect("lookup name_anonymous");
    let mk_str = module
        .exported::<(LeanName<'_>, String), LeanName<'_>>("lean_rs_fixture_name_mk_str")
        .expect("lookup name_mk_str");
    let demo_axiom = module
        .exported::<(LeanName<'_>,), LeanDeclaration<'_>>("lean_rs_fixture_declaration_demo_axiom")
        .expect("lookup declaration_demo_axiom");
    let render = module
        .exported::<(LeanDeclaration<'_>,), String>("lean_rs_fixture_declaration_name_to_string")
        .expect("lookup declaration_name_to_string");

    let name = mk_str
        .call(mk_anon.call(()).expect("anon"), "DemoAxiom".to_owned())
        .expect("mk DemoAxiom");
    let decl = demo_axiom.call(name).expect("demo_axiom");
    let rendered = render.call(decl).expect("render");

    assert!(
        rendered.starts_with("axiom"),
        "expected axiom-prefixed render, got {rendered:?}"
    );
    assert!(
        rendered.contains("DemoAxiom"),
        "expected name leaf in render, got {rendered:?}"
    );
}
