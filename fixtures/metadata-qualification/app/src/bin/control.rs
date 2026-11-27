//! Direct-host checks for the broken-import and optional WHNF fixtures.
//!
//! The qualifier runs the tests below with `cargo nextest run --bin control`.
//! The binary itself has no modes.
#[cfg(test)]
use metadata_qualification::Record;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    Err("control has no modes; the metadata qualifier runs its tests".into())
}

#[cfg(test)]
#[path = "../../../../../crates/lean-rs-host/tests/support/transitive_assertions.rs"]
mod transitive_assertions;

#[cfg(test)]
#[path = "../../../../../crates/lean-rs-host/tests/support/good_shims_assertions.rs"]
mod good_shims_assertions;

#[cfg(test)]
fn with_direct_shims(
    case: &str,
    project: &str,
    check: impl FnOnce(&lean_rs_host::LeanCapabilities<'static, '_>, &Record) -> Result<(), Box<dyn std::error::Error>>,
) -> Result<(), Box<dyn std::error::Error>> {
    use lean_rs::{LeanRuntime, module::LoadedHostArtifacts};
    use lean_rs_host::host::LeanPreparedHostArtifacts;
    use lean_rs_host::{LeanCapabilities, LeanHost};
    use metadata_qualification::Held;
    use std::sync::Arc;

    let held = Arc::new(Held {
        record: Record::selected(),
    });
    let r = &held.record;
    assert!(!r.stream && !r.verification);
    if case == "meta_missing_optional_symbol_returns_unsupported" {
        assert!(r.optional_whnf && r.typed_session && !r.broken_import && r.host_builder_case.is_none());
        // Keep the deployment record alive through process exit, even if a
        // native initializer or assertion fails.
        let _until_exit = Box::leak(Box::new(Arc::clone(&held)));
    } else {
        assert!(r.broken_import && !r.typed_session && !r.optional_whnf);
        assert_eq!(r.host_builder_case.as_deref(), Some(case));
    }
    assert_eq!(r.project.file_name().and_then(|name| name.to_str()), Some(project));
    held.verify()?;
    held.check_interop_visibility(false)?;

    let runtime = LeanRuntime::init()?;
    let host = LeanHost::from_lake_project(runtime, &r.project)?;
    let prepared =
        LeanPreparedHostArtifacts::from_snapshot(r.shim_manifest.clone(), r.shim_json.clone(), r.shim_paths.clone())?;
    // SAFETY: Held verified the exact sealed loader inputs, and no build
    // writes the consumer, path dependency or shims during the direct call.
    let loaded = unsafe {
        LoadedHostArtifacts::load(
            runtime,
            r.shim_manifest.clone(),
            r.shim_json.as_bytes().to_vec(),
            r.shim_paths.clone(),
            Arc::clone(&held),
        )?
    };
    held.check_interop_visibility(true)?;
    if r.optional_whnf {
        use lean_rs::module::{LeanLibrary, LeanModuleInitializer};
        // SAFETY: the ordinary fixture image is the existing selected SDK build,
        // sealed with the same runtime and both variant shims.
        let library = unsafe { LeanLibrary::open(runtime, &r.consumer_image) }?;
        let caps = LeanCapabilities::from_prepared_capability(
            &host,
            &prepared,
            loaded,
            (library, LeanModuleInitializer::new("lean_rs_fixture", "LeanRsFixture")),
            "lean_rs_fixture",
            "LeanRsFixture",
        )?;
        // The process can retain Lean registrations after the last local value.
        let caps = std::mem::ManuallyDrop::new(caps);
        held.check_optional_whnf_images()?;
        return check(&caps, r);
    }
    let caps = LeanCapabilities::from_prepared(&host, &prepared, loaded)?;
    check(&caps, r)?;
    Ok(())
}

#[test]
fn import_finds_transitive_lake_package_oleans() -> Result<(), Box<dyn std::error::Error>> {
    use lean_rs_host::{LeanImportProfileMode, LeanImportProfilerOptions, LeanSessionImportProfile};
    with_direct_shims(
        "shims_only_host_handle_imports_transitive_lake_package_oleans",
        "lean-transitive",
        |caps, r| {
            let paths = caps.import_search_paths();
            transitive_assertions::check(caps, |module| {
                let names = vec![module.to_owned()];
                let options = LeanImportProfileMode::FullSession(LeanSessionImportProfile::default())
                    .native_options(&LeanImportProfilerOptions::default());
                r.check_order(&paths, &names, &options)
                    .expect("exact sealed import graph");
                // SAFETY: check_order admits only the two reviewed modules, in this
                // exact order of sealed artifact roots and with default import options.
                unsafe {
                    caps.shim_capability()
                        .import_scope_with_options(paths.clone(), names, options)
                }
            });
            Ok(())
        },
    )?;
    metadata_qualification::audit("direct-host-transitive-passed");
    Ok(())
}

#[test]
fn load_shims_only_opens_session_against_user_oleans() -> Result<(), Box<dyn std::error::Error>> {
    use good_shims_assertions::{IMPORTS, KERNEL_SOURCE};
    use lean_rs::module::HostImportOptions;
    use lean_rs_host::LeanElabOptions;

    with_direct_shims("shims_only_session_opens_without_user_dylib", "lean-good", |caps, r| {
        let paths = caps.import_search_paths();
        assert!(
            paths
                .iter()
                .map(std::path::Path::new)
                .eq(r.import_roots.iter().map(std::path::PathBuf::as_path))
        );
        let names = IMPORTS.iter().map(|name| (*name).to_owned()).collect::<Vec<_>>();
        let options = HostImportOptions::default();
        // SAFETY: this direct-only request uses the exact reviewed module order
        // and default options over the verified Good and shim roots.
        let imports = unsafe {
            caps.shim_capability()
                .import_scope_with_options(paths.clone(), names.clone(), options.clone())
        };
        good_shims_assertions::check(caps, &imports, |session| {
            let (env, scope) = session.source_context();
            assert!(scope.matches_request(&paths, &names));
            assert_eq!(scope.options(), &options);
            // SAFETY: only the fixed theorem is admitted, in this same held
            // session environment with its checked imports and default options.
            let request = unsafe {
                caps.shim_capability().kernel_scope(
                    env,
                    scope,
                    r.verification_roots(),
                    KERNEL_SOURCE.into(),
                    LeanElabOptions::new().source_options(),
                )
            }?;
            session.kernel_check(&request, None, None)
        });
        Ok(())
    })?;
    metadata_qualification::audit("direct-host-good-passed");
    Ok(())
}

#[test]
fn load_shims_only_succeeds_when_user_shared_facet_does_not_build() -> Result<(), Box<dyn std::error::Error>> {
    use lean_rs::module::HostImportOptions;
    with_direct_shims(
        "load_shims_only_succeeds_when_user_shared_facet_does_not_build",
        "lean-broken-shared",
        |caps, r| {
            let paths = caps.import_search_paths();
            assert!(
                paths
                    .iter()
                    .map(std::path::Path::new)
                    .eq(r.import_roots.iter().map(std::path::PathBuf::as_path))
            );
            good_shims_assertions::check_broken_shared(caps, |name| {
                assert!(matches!(name, "Good" | "Broken"));
                // SAFETY: only the two original requests are admitted, with default
                // options and the same exact ordered, verified roots.
                unsafe {
                    caps.shim_capability().import_scope_with_options(
                        paths.clone(),
                        vec![name.to_owned()],
                        HostImportOptions::default(),
                    )
                }
            });
            Ok(())
        },
    )?;
    metadata_qualification::audit("direct-host-broken-shared-passed");
    Ok(())
}

#[test]
fn meta_missing_optional_symbol_returns_unsupported() -> Result<(), Box<dyn std::error::Error>> {
    use lean_rs::module::HostImportOptions;
    use lean_rs_host::LeanElabOptions;
    use lean_rs_host::host::meta::{LeanMetaOptions, MetaCallStatus, infer_type};

    with_direct_shims("meta_missing_optional_symbol_returns_unsupported", "lean", |caps, r| {
        let paths = caps.import_search_paths();
        assert!(
            paths
                .iter()
                .map(std::path::Path::new)
                .eq(r.import_roots.iter().map(std::path::PathBuf::as_path))
        );
        let names = ["LeanRsFixture.Meta".to_owned(), "LeanRsHostShims.Meta".to_owned()];
        let options = HostImportOptions::default();
        // SAFETY: these are the original two imports, in order and under
        // default options, using only the independently verified graph.
        let imports = unsafe {
            caps.shim_capability()
                .import_scope_with_options(paths.clone(), names.to_vec(), options.clone())
        };
        let mut session = caps.session(&imports, None, None)?;
        let (environment, scope) = session.source_context();
        assert!(scope.matches_request(&paths, &names));
        assert_eq!(scope.options(), &options);
        // SAFETY: only the original fixed Nat request, the same checked
        // environment and default options are used.
        let term = unsafe {
            caps.shim_capability().term_scope(
                environment,
                scope,
                r.verification_roots(),
                "Nat".into(),
                None,
                LeanElabOptions::new().source_options(),
            )
        }?;
        let expr = session
            .elaborate(&term, None)?
            .expect("fixture Nat expression elaborates");
        assert_eq!(
            session
                .run_meta(&infer_type(), expr.clone(), &LeanMetaOptions::new(), None)?
                .status(),
            MetaCallStatus::Ok,
        );
        good_shims_assertions::check_missing_whnf(&mut session, expr);
        Ok(())
    })?;
    metadata_qualification::audit("direct-host-optional-whnf-passed");
    Ok(())
}
