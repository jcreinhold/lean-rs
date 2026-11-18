pub(super) fn initializer_matches(
    approved: &lean_toolchain::LeanModuleInitializer,
    package: &str,
    module: &str,
) -> bool {
    approved.package_name() == package && approved.module_name() == module
}
