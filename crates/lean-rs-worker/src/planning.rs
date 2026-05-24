//! Import-set planning for worker-pool execution.
//!
//! The planner groups module work into stable worker session batches. It does
//! not choose workers, run commands, or define downstream cache semantics.

use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;

use lean_toolchain::{
    LeanLakeProjectModules, LeanModuleDescriptor, LeanModuleDiscoveryDiagnostic, LeanModuleDiscoveryOptions,
    LeanModuleSetFingerprint, ToolchainFingerprint, discover_lake_modules, lake_target_declared,
};
use serde_json::Value;

use crate::capability::LeanWorkerCapabilityBuilder;
use crate::pool::{LeanWorkerRestartPolicyClass, LeanWorkerSessionKey};
use crate::supervisor::LeanWorkerRestartPolicy;
use crate::types::LeanWorkerCapabilityMetadata;

/// Capability and session requirements used to plan worker batches.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LeanWorkerImportPlanConfig {
    project_root: PathBuf,
    package: String,
    lib_name: String,
    source_roots: Option<Vec<String>>,
    base_imports: Vec<String>,
    metadata_expectation: Option<LeanWorkerPlanMetadataExpectation>,
    restart_policy: Option<LeanWorkerRestartPolicy>,
}

impl LeanWorkerImportPlanConfig {
    /// Create planner configuration for a Lake capability target.
    #[must_use]
    pub fn new(project_root: impl Into<PathBuf>, package: impl Into<String>, lib_name: impl Into<String>) -> Self {
        Self {
            project_root: project_root.into(),
            package: package.into(),
            lib_name: lib_name.into(),
            source_roots: None,
            base_imports: Vec::new(),
            metadata_expectation: None,
            restart_policy: None,
        }
    }

    /// Restrict discovery to these source roots.
    #[must_use]
    pub fn source_roots(mut self, roots: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.source_roots = Some(roots.into_iter().map(Into::into).collect());
        self
    }

    /// Add imports required by every planned session batch.
    #[must_use]
    pub fn base_imports(mut self, imports: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.base_imports = imports.into_iter().map(Into::into).collect();
        self
    }

    /// Validate metadata before a worker batch is used.
    #[must_use]
    pub fn validate_metadata(mut self, export: impl Into<String>, request: Value) -> Self {
        self.metadata_expectation = Some(LeanWorkerPlanMetadataExpectation {
            export: export.into(),
            request,
            expected: None,
        });
        self
    }

    /// Require exact generic capability metadata before a worker batch is used.
    #[must_use]
    pub fn expect_metadata(
        mut self,
        export: impl Into<String>,
        request: Value,
        expected: LeanWorkerCapabilityMetadata,
    ) -> Self {
        self.metadata_expectation = Some(LeanWorkerPlanMetadataExpectation {
            export: export.into(),
            request,
            expected: Some(expected),
        });
        self
    }

    /// Use a worker restart policy for builders derived from planned batches.
    #[must_use]
    pub fn restart_policy(mut self, policy: LeanWorkerRestartPolicy) -> Self {
        self.restart_policy = Some(policy);
        self
    }
}

/// Metadata expectation carried into planned session keys.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LeanWorkerPlanMetadataExpectation {
    pub export: String,
    pub request: Value,
    pub expected: Option<LeanWorkerCapabilityMetadata>,
}

/// One module-sized unit of planned worker work.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LeanWorkerModuleWork {
    pub module: String,
    pub path: PathBuf,
    pub source_root: String,
    pub imports: Vec<String>,
}

impl LeanWorkerModuleWork {
    /// Create one module work item with explicit imports.
    #[must_use]
    pub fn new(
        module: impl Into<String>,
        path: impl Into<PathBuf>,
        source_root: impl Into<String>,
        imports: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        Self {
            module: module.into(),
            path: path.into(),
            source_root: source_root.into(),
            imports: imports.into_iter().map(Into::into).collect(),
        }
    }

    fn from_descriptor(descriptor: &LeanModuleDescriptor, imports: &[String]) -> Self {
        Self {
            module: descriptor.module.clone(),
            path: descriptor.path.clone(),
            source_root: descriptor.source_root.clone(),
            imports: imports.to_vec(),
        }
    }
}

/// One stable pool-execution batch.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LeanWorkerPlannedBatch {
    pub session_key: LeanWorkerSessionKey,
    pub project_root: PathBuf,
    pub package: String,
    pub lib_name: String,
    pub source_root: String,
    pub imports: Vec<String>,
    pub modules: Vec<LeanWorkerModuleWork>,
    pub fingerprint: LeanWorkerBatchFingerprint,
    metadata_expectation: Option<LeanWorkerPlanMetadataExpectation>,
    restart_policy: Option<LeanWorkerRestartPolicy>,
}

impl LeanWorkerPlannedBatch {
    /// Create a capability builder for this batch.
    ///
    /// The caller may still add packaging-specific details such as an explicit
    /// worker executable. The batch supplies the stable session material.
    #[must_use]
    pub fn capability_builder(&self) -> LeanWorkerCapabilityBuilder {
        let mut builder = LeanWorkerCapabilityBuilder::new(
            self.project_root.clone(),
            self.package.clone(),
            self.lib_name.clone(),
            self.imports.clone(),
        );
        if let Some(policy) = &self.restart_policy {
            builder = builder.restart_policy(policy.clone());
        }
        if let Some(expectation) = &self.metadata_expectation {
            builder = match &expectation.expected {
                Some(expected) => builder.expect_metadata(
                    expectation.export.clone(),
                    expectation.request.clone(),
                    expected.clone(),
                ),
                None => builder.validate_metadata(expectation.export.clone(), expectation.request.clone()),
            };
        }
        builder
    }
}

/// Stable cache-key-relevant facts for a planned batch.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LeanWorkerBatchFingerprint {
    pub toolchain: ToolchainFingerprint,
    pub source_set: LeanModuleSetFingerprint,
    pub batch_key: String,
}

/// Import planning diagnostics.
#[derive(Debug)]
pub enum LeanWorkerImportPlanError {
    ModuleDiscovery { diagnostic: LeanModuleDiscoveryDiagnostic },
    InvalidModuleName { module: String, reason: String },
    UnresolvedCapabilityTarget { project_root: PathBuf, target_name: String },
}

impl fmt::Display for LeanWorkerImportPlanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ModuleDiscovery { diagnostic } => write!(f, "{diagnostic}"),
            Self::InvalidModuleName { module, reason } => {
                write!(f, "lean-rs-worker: invalid module `{module}` in import plan: {reason}")
            }
            Self::UnresolvedCapabilityTarget {
                project_root,
                target_name,
            } => {
                write!(
                    f,
                    "lean-rs-worker: capability target `{target_name}` is not declared in {}",
                    project_root.display()
                )
            }
        }
    }
}

impl std::error::Error for LeanWorkerImportPlanError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::ModuleDiscovery { diagnostic } => Some(diagnostic),
            Self::InvalidModuleName { .. } | Self::UnresolvedCapabilityTarget { .. } => None,
        }
    }
}

/// Planner for worker-pool import/session batches.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LeanWorkerImportPlanner {
    config: LeanWorkerImportPlanConfig,
}

impl LeanWorkerImportPlanner {
    /// Create a planner from capability/session requirements.
    #[must_use]
    pub fn new(config: LeanWorkerImportPlanConfig) -> Self {
        Self { config }
    }

    /// Discover a Lake project and return stable worker batches.
    ///
    /// # Errors
    ///
    /// Returns typed planning diagnostics for missing Lake roots, selected
    /// module roots, invalid module names, unsupported toolchains, or an
    /// unresolved capability target.
    pub fn plan_lake_project(&self) -> Result<Vec<LeanWorkerPlannedBatch>, LeanWorkerImportPlanError> {
        let mut options = LeanModuleDiscoveryOptions::new(&self.config.project_root);
        if let Some(roots) = &self.config.source_roots {
            options = options.selected_roots(roots.clone());
        }
        let discovered = discover_lake_modules(options)
            .map_err(|diagnostic| LeanWorkerImportPlanError::ModuleDiscovery { diagnostic })?;
        let target_declared = lake_target_declared(&discovered.project_root, &self.config.lib_name)
            .map_err(|diagnostic| LeanWorkerImportPlanError::ModuleDiscovery { diagnostic })?;
        if !target_declared {
            return Err(LeanWorkerImportPlanError::UnresolvedCapabilityTarget {
                project_root: discovered.project_root,
                target_name: self.config.lib_name.clone(),
            });
        }
        self.plan_discovered(&discovered)
    }

    /// Plan batches from already discovered module descriptors.
    ///
    /// # Errors
    ///
    /// Returns a typed error if a supplied module descriptor has an invalid
    /// module name.
    pub fn plan_discovered(
        &self,
        discovered: &LeanLakeProjectModules,
    ) -> Result<Vec<LeanWorkerPlannedBatch>, LeanWorkerImportPlanError> {
        let work = discovered
            .modules
            .iter()
            .map(|module| LeanWorkerModuleWork::from_descriptor(module, &self.config.base_imports));
        self.plan_work_items(work, &discovered.fingerprint)
    }

    /// Plan batches from caller-provided module work items.
    ///
    /// # Errors
    ///
    /// Returns a typed error if a supplied work item has an invalid module
    /// name.
    pub fn plan_work_items(
        &self,
        modules: impl IntoIterator<Item = LeanWorkerModuleWork>,
        source_set: &LeanModuleSetFingerprint,
    ) -> Result<Vec<LeanWorkerPlannedBatch>, LeanWorkerImportPlanError> {
        let mut groups = BTreeMap::<BatchGroupKey, Vec<LeanWorkerModuleWork>>::new();
        for module in modules {
            validate_module_name(&module.module)?;
            validate_module_name(&module.source_root)?;
            let key = BatchGroupKey {
                project_root: self.config.project_root.clone(),
                package: self.config.package.clone(),
                lib_name: self.config.lib_name.clone(),
                source_root: module.source_root.clone(),
                imports: module.imports.clone(),
                restart_policy_class: restart_policy_class(self.config.restart_policy.as_ref()),
            };
            groups.entry(key).or_default().push(module);
        }

        let mut batches = Vec::new();
        for (key, mut modules) in groups {
            modules.sort_by(|left, right| left.module.cmp(&right.module));
            let mut session_key = LeanWorkerSessionKey::new(
                key.project_root.clone(),
                key.package.clone(),
                key.lib_name.clone(),
                key.imports.clone(),
            )
            .restart_policy_class(key.restart_policy_class);
            if let Some(expectation) = &self.config.metadata_expectation {
                session_key = session_key.metadata_expectation(
                    expectation.export.clone(),
                    expectation.request.clone(),
                    expectation.expected.clone(),
                );
            }
            let batch_key = batch_key_string(&key, &modules);
            batches.push(LeanWorkerPlannedBatch {
                session_key,
                project_root: key.project_root,
                package: key.package,
                lib_name: key.lib_name,
                source_root: key.source_root,
                imports: key.imports,
                modules,
                fingerprint: LeanWorkerBatchFingerprint {
                    toolchain: source_set.toolchain.clone(),
                    source_set: source_set.clone(),
                    batch_key,
                },
                metadata_expectation: self.config.metadata_expectation.clone(),
                restart_policy: self.config.restart_policy.clone(),
            });
        }
        Ok(batches)
    }
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct BatchGroupKey {
    project_root: PathBuf,
    package: String,
    lib_name: String,
    source_root: String,
    imports: Vec<String>,
    restart_policy_class: LeanWorkerRestartPolicyClass,
}

fn restart_policy_class(policy: Option<&LeanWorkerRestartPolicy>) -> LeanWorkerRestartPolicyClass {
    match policy {
        Some(policy) if policy == &LeanWorkerRestartPolicy::default() => LeanWorkerRestartPolicyClass::Default,
        Some(_policy) => LeanWorkerRestartPolicyClass::Custom,
        None => LeanWorkerRestartPolicyClass::Default,
    }
}

fn batch_key_string(key: &BatchGroupKey, modules: &[LeanWorkerModuleWork]) -> String {
    let module_list = modules
        .iter()
        .map(|module| module.module.as_str())
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "project={};package={};lib={};source_root={};imports={};policy={:?};modules={module_list}",
        key.project_root.display(),
        key.package,
        key.lib_name,
        key.source_root,
        key.imports.join(","),
        key.restart_policy_class,
    )
}

fn validate_module_name(module: &str) -> Result<(), LeanWorkerImportPlanError> {
    if module.is_empty() {
        return Err(LeanWorkerImportPlanError::InvalidModuleName {
            module: module.to_owned(),
            reason: "module name is empty".to_owned(),
        });
    }
    for component in module.split('.') {
        if component.is_empty() {
            return Err(LeanWorkerImportPlanError::InvalidModuleName {
                module: module.to_owned(),
                reason: "module name contains an empty component".to_owned(),
            });
        }
        let mut chars = component.chars();
        let Some(first) = chars.next() else {
            return Err(LeanWorkerImportPlanError::InvalidModuleName {
                module: module.to_owned(),
                reason: "module name contains an empty component".to_owned(),
            });
        };
        if !(first == '_' || first.is_alphabetic()) {
            return Err(LeanWorkerImportPlanError::InvalidModuleName {
                module: module.to_owned(),
                reason: "module components must begin with a letter or underscore".to_owned(),
            });
        }
        if chars.any(|ch| !(ch == '_' || ch == '\'' || ch.is_alphanumeric())) {
            return Err(LeanWorkerImportPlanError::InvalidModuleName {
                module: module.to_owned(),
                reason: "module components may contain only letters, digits, underscores, or apostrophes".to_owned(),
            });
        }
    }
    Ok(())
}
