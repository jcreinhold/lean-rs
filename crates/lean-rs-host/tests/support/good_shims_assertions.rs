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
