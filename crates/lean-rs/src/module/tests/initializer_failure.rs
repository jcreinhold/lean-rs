//! Exercise the generated Lean initializer and its actual loader aliases.
use super::native_image::{build_image, private_dir};
use super::{
    LeanCheckedExportError, LeanExportAbiRepr, LeanExportSignature, LeanLibrary, arg, fixture_manifest, ret, runtime,
};
use crate::LeanCapability;
use std::fs;

#[test]
fn failed_initializer_stays_failed_across_handles() {
    let dir = private_dir();
    let failed_image = build_image(
        &dir,
        "failed",
        &[
            (
                "InitProbe",
                r#"initialize failed : Nat ← throw (IO.userError "deliberate initializer error")"#,
            ),
            ("OtherProbe", ""),
        ],
    );
    let good_image = build_image(
        &dir,
        "good",
        &[(
            "InitProbe",
            "@[export d1_identity] def identityU8 (n : UInt8) : UInt8 := n",
        )],
    );

    #[allow(
        unsafe_code,
        reason = "open only the test-owned image from the selected runtime's compiler"
    )]
    // SAFETY: these two handles open the same immutable private image compiled above.
    // Its only initializer effects are a Lean IO error and standard Init setup.
    let (first, second) = unsafe {
        (
            LeanLibrary::open(runtime(), &failed_image).expect("first handle"),
            LeanLibrary::open(runtime(), &failed_image).expect("second handle"),
        )
    };
    let error = first
        .initialize_module("probe", "InitProbe")
        .expect_err("generated initializer throws");
    assert!(error.to_string().contains("deliberate initializer error"), "{error}");
    // Direct Lean compilation emits the package-independent spelling. Both
    // requested package names therefore resolve the same native initializer.
    let alias = first
        .initialize_module("another_package", "InitProbe")
        .expect_err("same native entry");
    assert_eq!(alias.to_string(), error.to_string());
    drop(first);
    let retry = second
        .initialize_module("probe", "InitProbe")
        .expect_err("another handle cannot turn a failed initializer into success");
    assert_eq!(retry.to_string(), error.to_string());
    second
        .initialize_module("probe", "OtherProbe")
        .expect("independent initializer in same image");
    assert!(second.resolve_function_symbol("d1_identity").is_err());

    let signature = LeanExportSignature::function(
        "d1_identity",
        vec![arg(LeanExportAbiRepr::U8)],
        ret(LeanExportAbiRepr::U8),
    );
    let manifest_path = dir.join("capability.json");
    let manifest = fixture_manifest(&good_image, "probe", "InitProbe", &[signature]);
    #[allow(unsafe_code, reason = "admit this test's private scalar-only Lean image")]
    // SAFETY: the captured manifest selects the second test-built image, its successful
    // initializer and its UInt8 identity. Both images and the runtime stay stable.
    let good = unsafe { LeanCapability::from_manifest_snapshot(runtime(), &manifest_path, &manifest) }
        .expect("same initializer spelling in an independent image");
    assert!(matches!(
        good.check_export_shape::<(u16,), u8>("d1_identity"),
        Err(LeanCheckedExportError::SignatureMismatch { .. })
    ));
    assert!(matches!(
        good.check_export_shape::<(u8,), u16>("d1_identity"),
        Err(LeanCheckedExportError::SignatureMismatch { .. })
    ));
    assert!(matches!(
        good.check_export_shape::<(u8,), u8>("missing_export"),
        Err(LeanCheckedExportError::MissingSignatureMetadata { .. })
    ));
    #[allow(unsafe_code, reason = "execute the exact UInt8 identity defined above")]
    // SAFETY: the selected export is the compiled UInt8 -> UInt8 function in the live good image.
    let value = unsafe {
        good.exported::<(u8,), u8>("d1_identity")
            .expect("declared export")
            .call(17)
    };
    assert_eq!(value.expect("scalar result"), 17);

    let wrong = fixture_manifest(&good_image, "probe", "OtherProbe", &[]);
    let manifest =
        lean_toolchain::CapabilityManifest::from_bytes(&manifest_path, &wrong).expect("well-formed manifest");
    let report = crate::module::preflight::LeanRuntimePreflight::check_manifest(&manifest_path, &manifest);
    assert!(
        report
            .checks()
            .iter()
            .any(|check| check.code() == crate::LeanLoaderDiagnosticCode::MissingInitializer)
    );
    fs::remove_dir_all(dir).expect("remove private generated files");
}
