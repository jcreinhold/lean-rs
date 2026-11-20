//! Shared assertions for the strict and admitted direct-host Good fixture.
use lean_rs::{LeanResult, module::TrustedHostImports};
use lean_rs_host::host::meta::{LeanMetaOptions, MetaCallStatus, infer_type};
use lean_rs_host::{EvidenceStatus, LeanCapabilities, LeanDeclarationFilter, LeanKernelOutcome, LeanSession};

pub(super) const IMPORTS: &[&str] = &["Good", "LeanRsHostShims.Meta"];
pub(super) const KERNEL_SOURCE: &str = "theorem good_kernel_check : 1 + 1 = 2 := rfl";

pub(super) fn check<'lean, 'cap>(
    caps: &'cap LeanCapabilities<'lean, '_>,
    imports: &TrustedHostImports<'lean, 'cap>,
    kernel_check: impl FnOnce(&mut LeanSession<'lean, 'cap>) -> LeanResult<LeanKernelOutcome<'lean>>,
) {
    let mut session = caps
        .session(imports, None, None)
        .expect("session imports user and shim oleans");

    let declarations = session
        .list_declarations_strings(&LeanDeclarationFilter::default(), None, None)
        .expect("list declarations works");
    assert!(
        declarations.iter().any(|name| name == "goodValue"),
        "user declaration should be visible in shim-only session"
    );

    let expr = session
        .declaration_type("goodValue", None)
        .expect("type query succeeds")
        .expect("goodValue has a type");
    let inferred = session
        .run_meta(&infer_type(), expr, &LeanMetaOptions::new(), None)
        .expect("infer_type dispatch succeeds");
    assert_eq!(inferred.status(), MetaCallStatus::Ok);

    let checked = kernel_check(&mut session).expect("kernel_check dispatch succeeds");
    assert_eq!(checked.status(), EvidenceStatus::Checked);
}

pub(super) fn check_broken_shared<'lean, 'cap>(
    caps: &'cap LeanCapabilities<'lean, '_>,
    import: impl Fn(&str) -> TrustedHostImports<'lean, 'cap>,
) {
    use lean_rs::error::LeanError;
    let mut good_session = caps
        .session(&import("Good"), None, None)
        .expect("unrelated module imports");
    let kind = good_session
        .declaration_kind("goodValue", None)
        .expect("declaration query works in unrelated module");
    assert_eq!(kind, "definition");

    let Err(broken_err) = caps.session(&import("Broken"), None, None) else {
        panic!("broken module import unexpectedly succeeded");
    };
    match broken_err {
        LeanError::LeanException(_) => {}
        LeanError::Host(failure) => panic!("expected LeanException for broken import, got Host {failure:?}"),
        LeanError::Cancelled(cancelled) => panic!("expected LeanException for broken import, got {cancelled:?}"),
    }
}

pub(super) fn check_missing_whnf<'lean>(session: &mut LeanSession<'lean, '_>, expr: lean_rs::LeanExpr<'lean>) {
    use lean_rs_host::host::meta::whnf;
    let outcome = session
        .run_meta(&whnf(), expr, &LeanMetaOptions::new(), None)
        .expect("optional service dispatch");
    assert_eq!(
        outcome.status(),
        MetaCallStatus::Unsupported,
        "missing optional meta symbol must return Unsupported, got {outcome:?}",
    );
}
