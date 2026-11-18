//! Round-trip tests for the four semantic handles.
//!
//! Each handle is exercised through the
//! [`crate::module::LeanModule::exported_unchecked`] dispatch path against the
//! `LeanRsFixture.Handles` capability. Construction and inspection happen
//! exclusively in Lean code; the Rust side only carries the handle.

#![allow(unsafe_code, clippy::expect_used, clippy::panic)]

use crate::fixed_test_library as fixture_library;

use crate::{LeanDeclaration, LeanExpr, LeanLevel, LeanName};

// -- LeanName ------------------------------------------------------------

#[test]
fn name_round_trips_through_lean_authored_display() {
    let library = fixture_library();
    let module = library
        .initialize_module("lean_rs_fixture", "LeanRsFixture")
        .expect("init root module");

    // SAFETY: the fixture/export signature is pinned by the Lean source for this call.
    let mk_anon = unsafe { module.exported_unchecked::<((),), LeanName<'_>>("lean_rs_fixture_name_anonymous") }
        .expect("lookup name_anonymous");
    // SAFETY: the fixture/export signature is pinned by the Lean source for this call.
    let mk_str =
        unsafe { module.exported_unchecked::<(LeanName<'_>, String), LeanName<'_>>("lean_rs_fixture_name_mk_str") }
            .expect("lookup name_mk_str");
    // SAFETY: the fixture/export signature is pinned by the Lean source for this call.
    let to_str = unsafe { module.exported_unchecked::<(LeanName<'_>,), String>("lean_rs_fixture_name_to_string") }
        .expect("lookup name_to_string");

    let root = (
        // SAFETY: the fixture manufactures the semantic handles in this test; each is consumed at its declared Lean type.
        unsafe { mk_anon.call(()) }
    )
    .expect("call name_anonymous");
    let child = (
        // SAFETY: the fixture manufactures the semantic handles in this test; each is consumed at its declared Lean type.
        unsafe { mk_str.call(root, "Demo".to_owned()) }
    )
    .expect("call name_mk_str");
    let rendered = (
        // SAFETY: the fixture manufactures the semantic handles in this test; each is consumed at its declared Lean type.
        unsafe { to_str.call(child) }
    )
    .expect("call name_to_string");

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

    // SAFETY: the fixture/export signature is pinned by the Lean source for this call.
    let mk_anon = unsafe { module.exported_unchecked::<((),), LeanName<'_>>("lean_rs_fixture_name_anonymous") }
        .expect("lookup name_anonymous");
    // SAFETY: the fixture/export signature is pinned by the Lean source for this call.
    let mk_str =
        unsafe { module.exported_unchecked::<(LeanName<'_>, String), LeanName<'_>>("lean_rs_fixture_name_mk_str") }
            .expect("lookup name_mk_str");
    // SAFETY: the fixture/export signature is pinned by the Lean source for this call.
    let beq = unsafe { module.exported_unchecked::<(LeanName<'_>, LeanName<'_>), bool>("lean_rs_fixture_name_beq") }
        .expect("lookup name_beq");

    let a = (
        // SAFETY: the fixture manufactures the semantic handles in this test; each is consumed at its declared Lean type.
        unsafe { mk_str.call(mk_anon.call(()).expect("anon a"), "Foo".to_owned()) }
    )
    .expect("mk a");
    let b = (
        // SAFETY: the fixture manufactures the semantic handles in this test; each is consumed at its declared Lean type.
        unsafe { mk_str.call(mk_anon.call(()).expect("anon b"), "Foo".to_owned()) }
    )
    .expect("mk b");
    let c = (
        // SAFETY: the fixture manufactures the semantic handles in this test; each is consumed at its declared Lean type.
        unsafe { mk_str.call(mk_anon.call(()).expect("anon c"), "Bar".to_owned()) }
    )
    .expect("mk c");

    assert!(
        (
            // SAFETY: the fixture manufactures the semantic handles in this test; each is consumed at its declared Lean type.
            unsafe { beq.call(a.clone(), b) }
        )
        .expect("beq equal")
    );
    assert!(
        !(
            // SAFETY: the fixture manufactures the semantic handles in this test; each is consumed at its declared Lean type.
            unsafe { beq.call(a, c) }
        )
        .expect("beq differ")
    );
}

// -- LeanLevel -----------------------------------------------------------

#[test]
fn level_round_trips_succ_zero() {
    let library = fixture_library();
    let module = library
        .initialize_module("lean_rs_fixture", "LeanRsFixture")
        .expect("init root module");

    // SAFETY: the fixture/export signature is pinned by the Lean source for this call.
    let zero = unsafe { module.exported_unchecked::<((),), LeanLevel<'_>>("lean_rs_fixture_level_zero") }
        .expect("lookup level_zero");
    // SAFETY: the fixture/export signature is pinned by the Lean source for this call.
    let succ = unsafe { module.exported_unchecked::<(LeanLevel<'_>,), LeanLevel<'_>>("lean_rs_fixture_level_succ") }
        .expect("lookup level_succ");
    // SAFETY: the fixture/export signature is pinned by the Lean source for this call.
    let to_str = unsafe { module.exported_unchecked::<(LeanLevel<'_>,), String>("lean_rs_fixture_level_to_string") }
        .expect("lookup level_to_string");

    let one = (
        // SAFETY: the fixture manufactures the semantic handles in this test; each is consumed at its declared Lean type.
        unsafe { succ.call(zero.call(()).expect("zero")) }
    )
    .expect("succ zero");
    let rendered = (
        // SAFETY: the fixture manufactures the semantic handles in this test; each is consumed at its declared Lean type.
        unsafe { to_str.call(one) }
    )
    .expect("level_to_string");

    // Lean prints `succ zero` as `"1"`; assert a non-empty render and a
    // digit somewhere so the test tolerates pretty-printer
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

    // SAFETY: the fixture/export signature is pinned by the Lean source for this call.
    let zero = unsafe { module.exported_unchecked::<((),), LeanLevel<'_>>("lean_rs_fixture_level_zero") }
        .expect("lookup level_zero");
    // SAFETY: the fixture/export signature is pinned by the Lean source for this call.
    let succ = unsafe { module.exported_unchecked::<(LeanLevel<'_>,), LeanLevel<'_>>("lean_rs_fixture_level_succ") }
        .expect("lookup level_succ");
    // SAFETY: the fixture/export signature is pinned by the Lean source for this call.
    let beq = unsafe { module.exported_unchecked::<(LeanLevel<'_>, LeanLevel<'_>), bool>("lean_rs_fixture_level_beq") }
        .expect("lookup level_beq");

    let a = (
        // SAFETY: the fixture manufactures the semantic handles in this test; each is consumed at its declared Lean type.
        unsafe { succ.call(zero.call(()).expect("zero a")) }
    )
    .expect("succ a");
    let b = (
        // SAFETY: the fixture manufactures the semantic handles in this test; each is consumed at its declared Lean type.
        unsafe { succ.call(zero.call(()).expect("zero b")) }
    )
    .expect("succ b");
    let c = (
        // SAFETY: the fixture manufactures the semantic handles in this test; each is consumed at its declared Lean type.
        unsafe { zero.call(()) }
    )
    .expect("zero c");

    assert!(
        (
            // SAFETY: the fixture manufactures the semantic handles in this test; each is consumed at its declared Lean type.
            unsafe { beq.call(a.clone(), b) }
        )
        .expect("beq equal")
    );
    assert!(
        !(
            // SAFETY: the fixture manufactures the semantic handles in this test; each is consumed at its declared Lean type.
            unsafe { beq.call(a, c) }
        )
        .expect("beq differ")
    );
}

// -- LeanExpr ------------------------------------------------------------

#[test]
fn expr_const_nat_round_trips() {
    let library = fixture_library();
    let module = library
        .initialize_module("lean_rs_fixture", "LeanRsFixture")
        .expect("init root module");

    // SAFETY: the fixture/export signature is pinned by the Lean source for this call.
    let const_nat = unsafe { module.exported_unchecked::<((),), LeanExpr<'_>>("lean_rs_fixture_expr_const_nat") }
        .expect("lookup expr_const_nat");
    // SAFETY: the fixture/export signature is pinned by the Lean source for this call.
    let to_str = unsafe { module.exported_unchecked::<(LeanExpr<'_>,), String>("lean_rs_fixture_expr_to_string") }
        .expect("lookup expr_to_string");

    let rendered = (
        // SAFETY: the fixture manufactures the semantic handles in this test; each is consumed at its declared Lean type.
        unsafe { to_str.call(const_nat.call(()).expect("const Nat")) }
    )
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

    // SAFETY: the fixture/export signature is pinned by the Lean source for this call.
    let const_nat = unsafe { module.exported_unchecked::<((),), LeanExpr<'_>>("lean_rs_fixture_expr_const_nat") }
        .expect("lookup expr_const_nat");
    // SAFETY: the fixture/export signature is pinned by the Lean source for this call.
    let app =
        unsafe { module.exported_unchecked::<(LeanExpr<'_>, LeanExpr<'_>), LeanExpr<'_>>("lean_rs_fixture_expr_app") }
            .expect("lookup expr_app");
    // SAFETY: the fixture/export signature is pinned by the Lean source for this call.
    let bvar = unsafe { module.exported_unchecked::<(u64,), LeanExpr<'_>>("lean_rs_fixture_expr_bvar") }
        .expect("lookup expr_bvar");

    let f = (
        // SAFETY: the fixture manufactures the semantic handles in this test; each is consumed at its declared Lean type.
        unsafe { const_nat.call(()) }
    )
    .expect("f");
    let x = (
        // SAFETY: the fixture manufactures the semantic handles in this test; each is consumed at its declared Lean type.
        unsafe { bvar.call(0) }
    )
    .expect("x");
    let applied = (
        // SAFETY: the fixture manufactures the semantic handles in this test; each is consumed at its declared Lean type.
        unsafe { app.call(f, x) }
    )
    .expect("expr_app");
    // Round-tripped through Lean: passing it back through expr_app
    // should not error and should produce another distinct expression.
    let again = (
        // SAFETY: the fixture manufactures the semantic handles in this test; each is consumed at its declared Lean type.
        unsafe { app.call(applied.clone(), bvar.call(1).expect("y")) }
    )
    .expect("expr_app second time");
    drop(again);
}

#[test]
fn expr_equality_via_fixture_export() {
    let library = fixture_library();
    let module = library
        .initialize_module("lean_rs_fixture", "LeanRsFixture")
        .expect("init root module");

    // SAFETY: the fixture/export signature is pinned by the Lean source for this call.
    let const_nat = unsafe { module.exported_unchecked::<((),), LeanExpr<'_>>("lean_rs_fixture_expr_const_nat") }
        .expect("lookup expr_const_nat");
    // SAFETY: the fixture/export signature is pinned by the Lean source for this call.
    let bvar = unsafe { module.exported_unchecked::<(u64,), LeanExpr<'_>>("lean_rs_fixture_expr_bvar") }
        .expect("lookup expr_bvar");
    // SAFETY: the fixture/export signature is pinned by the Lean source for this call.
    let beq = unsafe { module.exported_unchecked::<(LeanExpr<'_>, LeanExpr<'_>), bool>("lean_rs_fixture_expr_beq") }
        .expect("lookup expr_beq");

    let a = (
        // SAFETY: the fixture manufactures the semantic handles in this test; each is consumed at its declared Lean type.
        unsafe { const_nat.call(()) }
    )
    .expect("a");
    let b = (
        // SAFETY: the fixture manufactures the semantic handles in this test; each is consumed at its declared Lean type.
        unsafe { const_nat.call(()) }
    )
    .expect("b");
    let c = (
        // SAFETY: the fixture manufactures the semantic handles in this test; each is consumed at its declared Lean type.
        unsafe { bvar.call(0) }
    )
    .expect("c");

    assert!(
        (
            // SAFETY: the fixture manufactures the semantic handles in this test; each is consumed at its declared Lean type.
            unsafe { beq.call(a.clone(), b) }
        )
        .expect("beq equal")
    );
    assert!(
        !(
            // SAFETY: the fixture manufactures the semantic handles in this test; each is consumed at its declared Lean type.
            unsafe { beq.call(a, c) }
        )
        .expect("beq differ")
    );
}

// -- LeanDeclaration -----------------------------------------------------

#[test]
fn declaration_demo_axiom_round_trips() {
    let library = fixture_library();
    let module = library
        .initialize_module("lean_rs_fixture", "LeanRsFixture")
        .expect("init root module");

    // SAFETY: the fixture/export signature is pinned by the Lean source for this call.
    let mk_anon = unsafe { module.exported_unchecked::<((),), LeanName<'_>>("lean_rs_fixture_name_anonymous") }
        .expect("lookup name_anonymous");
    // SAFETY: the fixture/export signature is pinned by the Lean source for this call.
    let mk_str =
        unsafe { module.exported_unchecked::<(LeanName<'_>, String), LeanName<'_>>("lean_rs_fixture_name_mk_str") }
            .expect("lookup name_mk_str");
    // SAFETY: the fixture/export signature is pinned by the Lean source for this call.
    let demo_axiom = unsafe {
        module.exported_unchecked::<(LeanName<'_>,), LeanDeclaration<'_>>("lean_rs_fixture_declaration_demo_axiom")
    }
    .expect("lookup declaration_demo_axiom");
    // SAFETY: the fixture/export signature is pinned by the Lean source for this call.
    let render = unsafe {
        module.exported_unchecked::<(LeanDeclaration<'_>,), String>("lean_rs_fixture_declaration_name_to_string")
    }
    .expect("lookup declaration_name_to_string");

    let name = (
        // SAFETY: the fixture manufactures the semantic handles in this test; each is consumed at its declared Lean type.
        unsafe { mk_str.call(mk_anon.call(()).expect("anon"), "DemoAxiom".to_owned()) }
    )
    .expect("mk DemoAxiom");
    let decl = (
        // SAFETY: the fixture manufactures the semantic handles in this test; each is consumed at its declared Lean type.
        unsafe { demo_axiom.call(name) }
    )
    .expect("demo_axiom");
    let rendered = (
        // SAFETY: the fixture manufactures the semantic handles in this test; each is consumed at its declared Lean type.
        unsafe { render.call(decl) }
    )
    .expect("render");

    assert!(
        rendered.starts_with("axiom"),
        "expected axiom-prefixed render, got {rendered:?}"
    );
    assert!(
        rendered.contains("DemoAxiom"),
        "expected name leaf in render, got {rendered:?}"
    );
}
