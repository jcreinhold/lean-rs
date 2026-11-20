use lean_rs::LeanBuiltCapability;
use lean_rs_worker_parent::{
    LeanWorkerCapabilityBuilder, LeanWorkerCapabilityMetadata, LeanWorkerChild, LeanWorkerError,
};
use metadata_qualification::{EXPORT, MODULE, Record};
use serde_json::json;
use std::{path::PathBuf, time::Duration};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mode = std::env::args().nth(1).ok_or("mode")?;
    let d = Record::selected();
    if mode == "collect-closed" {
        let selected = PathBuf::from(
            option_env!("LEAN_RS_METADATA_DEPLOYMENT_RECORD").ok_or("missing selected deployment record")?,
        );
        let expected = selected
            .parent()
            .ok_or("selected deployment has no state directory")?
            .join("owners");
        let owners = PathBuf::from(std::env::var_os("LEAN_RS_WORKER_OWNERS_DIRECTORY").ok_or("missing owners")?);
        if owners != expected || owners != owners.canonicalize()? {
            return Err("collector owners differ from the reviewed disposable deployment".into());
        }
        lean_toolchain::native_guardian::collect_closed_owner_records(&owners, Duration::from_secs(2))?;
        let mut entries = std::fs::read_dir(&owners)?;
        let entry = entries.next().ok_or("missing admission lock")??;
        if entry.file_name() != ".admission.lock" || entries.next().is_some() {
            return Err("closed collector left an unexpected registry entry".into());
        }
        println!("closed owner records collected");
        return Ok(());
    }
    let mut prepared = d.descriptor();
    if mode == "reversed" {
        prepared.shim_search_paths.reverse();
    }
    // This application supplies this exact DTO to the SDK launcher for every open.
    // Compare the candidate with the independent build record before it can spawn a child.
    if prepared != d.descriptor() {
        if mode == "reversed" {
            println!("rejected reordered prepared roots before worker construction");
            return Ok(());
        }
        return Err("unknown prepared deployment".into());
    }
    let worker = std::env::current_exe()?.with_file_name("metadata-worker");
    let owners = PathBuf::from(std::env::var_os("LEAN_RS_WORKER_OWNERS_DIRECTORY").ok_or("missing owners")?);
    let state = owners.parent().ok_or("owners have no state directory")?;
    let qualification = state.parent().ok_or("state has no qualification directory")?;
    let cargo_target = qualification.parent().ok_or("state is not below the Cargo target")?;
    if owners.file_name().and_then(|name| name.to_str()) != Some("owners")
        || state.file_name().and_then(|name| name.to_str()) != Some("ordinary")
        || qualification.file_name().and_then(|name| name.to_str()) != Some("metadata-qualification")
    {
        return Err("owners path is outside the ordinary metadata state".into());
    }
    let socket_root = cargo_target.join("ws");
    let child = LeanWorkerChild::path(worker)
        .shared_writer_executable(Record::helper())
        .native_guardian(Record::helper())
        .lifetime_directory(owners)
        .lifetime_socket_directory(socket_root)
        .prepared_host_artifacts(prepared)
        .shared_artifact_reads(true);
    let target = LeanBuiltCapability::manifest_path(&d.consumer_manifest);
    let expected: LeanWorkerCapabilityMetadata = serde_json::from_value(json!({
        "commands":[{"name":"version","version":"fixture-1"},{"name":"scan","version":"fixture-2"}],
        "capabilities":[{"name":"rows.json","version":"fixture-1"},{"name":"diagnostics","version":"fixture-1"}],
        "lean_version":"fixture-lean-4","extra":{"fixture":true}}))?;
    let mut selected = expected.clone();
    if mode == "mismatch" {
        selected.commands[0].version = "deliberately-wrong".into()
    }
    let builder = LeanWorkerCapabilityBuilder::from_built_capability(&target, [MODULE])?
        .worker_child(child)
        .request_timeout(Duration::from_secs(100))
        .startup_timeout(Duration::from_secs(40))
        .require_host_runtime(Default::default())
        .expect_metadata(EXPORT, json!({"caller":"private-metadata-fixture"}), selected.clone());
    match (mode.as_str(), builder.open()) {
        ("positive", Ok(cap)) => {
            assert_eq!(cap.validated_metadata(), Some(&expected));
            let mut effective: Vec<_> = d.import_roots.iter().map(|p| p.to_str().unwrap().to_owned()).collect();
            effective.push(d.sysroot.join("lib/lean").to_str().unwrap().to_owned());
            let runtime = cap.host_runtime_metadata().ok_or("missing runtime metadata")?;
            assert_eq!(runtime.effective_import_roots.as_ref(), Some(&effective));
            let mut domains: Vec<_> = runtime
                .artifact_read_epochs
                .as_ref()
                .ok_or("missing epochs")?
                .iter()
                .map(|e| PathBuf::from(&e.project_root))
                .collect();
            domains.sort();
            assert_eq!(domains, d.roots);
            println!(
                "complete native metadata {}\nordered roots {}\nepochs {}",
                serde_json::to_string(&expected)?,
                serde_json::to_string(&effective)?,
                serde_json::to_string(&runtime.artifact_read_epochs)?
            );
            println!("shutdown {:?}", cap.shutdown()?.outcome);
        }
        (
            "mismatch",
            Err(LeanWorkerError::CapabilityMetadataMismatch {
                export,
                expected: wanted,
                actual,
            }),
        ) => {
            assert_eq!(export, EXPORT);
            assert_eq!(*wanted, selected);
            assert_eq!(*actual, expected);
            println!(
                "CapabilityMetadataMismatch after exact export; actual {}",
                serde_json::to_string(&actual)?
            );
        }
        ("reversed" | "wrong-image" | "wrong-initializer" | "wrong-ordered-directories", Err(e)) => {
            println!("refusal {e:?}")
        }
        (_, Err(e)) => return Err(format!("unexpected result: {e:?}").into()),
        (_, Ok(_)) => return Err("unexpected success".into()),
    }
    Ok(())
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

    let held = Arc::new(Held::acquire(Record::selected())?);
    let r = &held.record;
    assert!(!r.stream && !r.verification);
    if case == "meta_missing_optional_symbol_returns_unsupported" {
        assert!(r.optional_whnf && r.typed_session && !r.broken_import && r.host_builder_case.is_none());
        // Keep the exact private graph and existing fixture held through
        // process exit, even if a native initializer or assertion fails.
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
    let prepared = LeanPreparedHostArtifacts::from_snapshot(
        r.shim_manifest.clone(),
        r.shim_json.clone(),
        r.shim_roots.clone(),
        r.shim_paths.clone(),
    )?;
    // SAFETY: Held verified the exact sealed loader inputs and retains the
    // consumer, path dependency and both shim locks across the direct call.
    let loaded = unsafe {
        LoadedHostArtifacts::load(
            runtime,
            r.shim_manifest.clone(),
            r.shim_json.as_bytes().to_vec(),
            r.shim_roots.clone(),
            r.shim_paths.clone(),
            Arc::clone(&held),
        )?
    };
    held.check_interop_visibility(true)?;
    if r.optional_whnf {
        use lean_rs::module::{LeanLibrary, LeanModuleInitializer};
        // SAFETY: the ordinary fixture image is the existing selected SDK build,
        // sealed with the same runtime and held alongside both variant shims.
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
        assert_eq!(caps.artifact_roots(), r.roots);
        held.check_optional_whnf_images()?;
        return check(&caps, r);
    }
    let caps = LeanCapabilities::from_prepared(&host, &prepared, loaded)?;
    assert_eq!(caps.artifact_roots(), r.roots);
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
                // exact order of held artifact roots and with default import options.
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
        // and default options while holding the verified Good and shim roots.
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
                // options and the same exact ordered roots under held read locks.
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
        // default options, using only the independently held graph.
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
