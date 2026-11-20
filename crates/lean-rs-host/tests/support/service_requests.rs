//! Request admission for the closed host-services integration binary.
//! Policy rows are installed by the reviewing build, independently of test inputs.
use lean_rs::host_bridge::HostSourceOptions;
use lean_rs::module::{HostImportOptions, TrustedBracketedImports, TrustedHostImports};
use lean_rs_host::{LeanCapabilities, LeanSession, LeanSessionImportProfile};

pub(super) fn imports<'lean, 'c>(
    caps: &'c LeanCapabilities<'lean, '_>,
    names: &[&str],
    profile: LeanSessionImportProfile,
) -> TrustedHostImports<'lean, 'c> {
    let options = lean_rs_host::LeanImportProfileMode::FullSession(profile)
        .native_options(&lean_rs_host::LeanImportProfilerOptions::default());
    let roots = super::fixture::approved_graph(caps, names, &options, "host-services/import");
    #[allow(
        unsafe_code,
        reason = "the build policy pins this graph, profile and artifact inventory"
    )]
    // SAFETY: the protected policy row covers the complete graph under retained reader custody.
    unsafe {
        caps.shim_capability()
            .import_scope_with_options(roots, names.iter().map(|s| (*s).into()).collect(), options)
    }
}

pub(super) fn bracketed<'lean, 'c>(
    caps: &'c LeanCapabilities<'lean, '_>,
    names: &[&str],
) -> TrustedBracketedImports<'lean, 'c> {
    let roots = if names == ["LeanRsFixture.Handles"] {
        super::fixture::native_bracketed_handles_paths(caps)
    } else {
        None
    }
    .unwrap_or_else(|| {
        super::fixture::approved_graph(caps, names, &HostImportOptions::default(), "host-services/bracketed")
    });
    #[allow(unsafe_code, reason = "separate protected authorization for region release")]
    // SAFETY: the reviewed row or the held ordinary fixture supplies the exact
    // graph and images. The same bracketed scope controls import and cleanup.
    unsafe {
        caps.shim_capability()
            .bracketed_import_scope(roots, names.iter().map(|s| (*s).into()).collect())
    }
}

pub(super) fn approved_paths(
    caps: &LeanCapabilities<'_, '_>,
    session: &LeanSession<'_, '_>,
    operation: &str,
    payload: &str,
    options: &HostSourceOptions,
) -> Vec<String> {
    let (_, scope) = session.source_context();
    scope
        .check_capability(caps.shim_capability())
        .expect("same loaded capability");
    assert!(scope.matches_request(&caps.import_search_paths(), scope.imports()));
    let names = scope.imports().iter().map(String::as_str).collect::<Vec<_>>();
    let description = serde_json::json!([
        operation,
        payload,
        options.namespace,
        options.file,
        options.heartbeats,
        options.diagnostic_bytes,
        options.options_json
    ])
    .to_string();
    let mut paths = super::fixture::approved_graph(caps, &names, scope.options(), &description);
    paths.push(super::fixture::release_import_path());
    paths
}
