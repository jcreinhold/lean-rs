//! Shared TOML lakefile parser used by both `build_helpers` (link-time target
//! resolution) and `modules` (Lake module discovery).
//!
//! The parser is intentionally diagnostic-agnostic: it returns a small typed
//! shape plus `toml::de::Error` on parse failure, and each caller maps that
//! error into its own [`crate::LinkDiagnostics`] or
//! [`crate::modules::LeanModuleDiscoveryDiagnostic`] variant. That keeps the
//! parser one place while preserving each module's error contract.

use toml::Value;

/// Shape extracted from a Lake `lakefile.toml`.
#[derive(Debug)]
pub(crate) struct LakefileToml {
    /// Top-level `name = "..."` package name, if present and non-empty.
    pub(crate) package_name: Option<String>,
    /// `name` of every `[[lean_lib]]` array-of-table entry, in source order,
    /// with non-string or empty values skipped.
    pub(crate) lean_libs: Vec<String>,
}

/// Parse a Lake TOML lakefile.
///
/// Errors only when the input is not valid TOML; missing or unexpectedly-typed
/// fields produce empty entries in the returned shape, mirroring the
/// permissive behaviour Lake itself applies to incomplete lakefiles.
pub(crate) fn parse_lakefile_toml(text: &str) -> Result<LakefileToml, toml::de::Error> {
    let document: Value = toml::from_str(text)?;
    let package_name = document
        .get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_owned);
    let lean_libs = document
        .get("lean_lib")
        .and_then(Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| entry.as_table())
                .filter_map(|table| table.get("name").and_then(Value::as_str))
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    Ok(LakefileToml {
        package_name,
        lean_libs,
    })
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic)]
mod tests {
    use super::parse_lakefile_toml;

    #[test]
    fn parses_package_name_and_lean_libs() {
        let parsed = parse_lakefile_toml(
            r#"
name = "demo"
defaultTargets = ["FixtureLib"]

[[lean_lib]]
name = "FixtureLib"

[[lean_lib]]
name = "Other"
"#,
        )
        .expect("valid TOML");
        assert_eq!(parsed.package_name.as_deref(), Some("demo"));
        assert_eq!(parsed.lean_libs, vec!["FixtureLib", "Other"]);
    }

    #[test]
    fn missing_name_yields_none() {
        let parsed = parse_lakefile_toml("[[lean_lib]]\nname = \"Only\"\n").expect("valid TOML");
        assert!(parsed.package_name.is_none());
        assert_eq!(parsed.lean_libs, vec!["Only"]);
    }

    #[test]
    fn empty_lean_lib_entries_are_skipped() {
        let parsed = parse_lakefile_toml(
            r#"
name = "demo"

[[lean_lib]]
name = ""

[[lean_lib]]
name = "Real"
"#,
        )
        .expect("valid TOML");
        assert_eq!(parsed.lean_libs, vec!["Real"]);
    }

    #[test]
    fn invalid_toml_errors() {
        let err = parse_lakefile_toml("name = ").expect_err("invalid TOML");
        let rendered = err.to_string();
        assert!(!rendered.is_empty());
    }

    #[test]
    fn lean_lib_without_name_is_skipped() {
        let parsed = parse_lakefile_toml(
            r#"
name = "demo"

[[lean_lib]]
defaultFacets = ["LeanLib.sharedFacet"]
"#,
        )
        .expect("valid TOML");
        assert!(parsed.lean_libs.is_empty());
    }
}
