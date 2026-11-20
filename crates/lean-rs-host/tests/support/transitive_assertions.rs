//! Shared assertions for the strict and admitted direct-host transitive import.
use lean_rs::{LeanError, module::TrustedHostImports};
use lean_rs_host::{LeanCapabilities, LeanDeclarationFilter};

pub(super) fn check<'c>(
    caps: &'c LeanCapabilities<'static, '_>,
    import: impl Fn(&str) -> TrustedHostImports<'static, 'c>,
) {
    let mut session = caps
        .session(&import("Consumer"), None, None)
        .expect("consumer imports dependency");
    let declarations = session
        .list_declarations_strings(&LeanDeclarationFilter::default(), None, None)
        .expect("list declarations works");
    assert!(
        declarations.iter().any(|name| name.starts_with("Dep.")),
        "dependency declaration should be visible when transitive oleans are on the search path"
    );

    let Err(missing_err) = caps.session(&import("Dep.NonExistent"), None, None) else {
        panic!("missing dependency module unexpectedly imported");
    };
    match missing_err {
        LeanError::LeanException(exc) => {
            assert!(
                exc.message().contains("Dep.NonExistent"),
                "missing module should be reported by Lean import, got: {}",
                exc.message(),
            );
        }
        LeanError::Host(failure) => panic!("expected LeanException for missing import, got Host {failure:?}"),
        LeanError::Cancelled(cancelled) => panic!("expected LeanException for missing import, got {cancelled:?}"),
    }
}
