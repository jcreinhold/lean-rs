#[path = "../src/host/project_initializer.rs"]
mod project_initializer;

#[test]
fn project_initializer_requires_actual_package_and_module() {
    let approved = lean_toolchain::LeanModuleInitializer::new("pkg", "Project.Main");
    assert!(project_initializer::initializer_matches(&approved, "pkg", "Project.Main"));
    assert!(!project_initializer::initializer_matches(&approved, "other", "Project.Main"));
    assert!(!project_initializer::initializer_matches(&approved, "pkg", "Other.Main"));
}
