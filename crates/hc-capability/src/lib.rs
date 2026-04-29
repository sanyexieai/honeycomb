//! Capability profiles and sharing rules.

use anyhow::Result;
use hc_store::store::{StoredMarkdown, WorkspaceNamespace, WorkspaceStore};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CapabilityNamespace {
    pub tenant_id: String,
    pub user_id: String,
}

impl CapabilityNamespace {
    pub fn new(tenant_id: impl Into<String>, user_id: impl Into<String>) -> Self {
        Self {
            tenant_id: tenant_id.into(),
            user_id: user_id.into(),
        }
    }

    pub fn local_default() -> Self {
        Self::new("local", "default")
    }
}

impl Default for CapabilityNamespace {
    fn default() -> Self {
        Self::local_default()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityVisibility {
    Private,
    TenantShared,
    CrossTenantShared,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityInputType {
    NaturalLanguage,
    StructuredTask,
    MarkdownDocument,
    SessionMessage,
    CommandRequest,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityOutputType {
    ChatReply,
    Summary,
    Decision,
    TaskPlan,
    ToolInvocation,
    ReviewNotes,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityTier {
    #[default]
    KnowledgeInterface,
    RuntimeFoundation,
    AtomicUnit,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ModelDependence {
    #[default]
    Required,
    Optional,
    None,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CapabilityProfile {
    pub id: String,
    #[serde(default)]
    pub namespace: CapabilityNamespace,
    #[serde(default = "default_capability_visibility")]
    pub visibility: CapabilityVisibility,
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub tier: CapabilityTier,
    #[serde(default)]
    pub model_dependence: ModelDependence,
    pub domains: Vec<String>,
    pub skills: Vec<String>,
    pub input_types: Vec<CapabilityInputType>,
    pub output_types: Vec<CapabilityOutputType>,
    pub tool_refs: Vec<String>,
    pub workflow_refs: Vec<String>,
    pub dependency_refs: Vec<String>,
    pub optimization_of_refs: Vec<String>,
    pub constraints: Vec<String>,
    pub tags: Vec<String>,
}

impl CapabilityProfile {
    pub fn new(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            namespace: CapabilityNamespace::local_default(),
            visibility: default_capability_visibility(),
            name: name.into(),
            description: String::new(),
            tier: CapabilityTier::KnowledgeInterface,
            model_dependence: ModelDependence::Required,
            domains: Vec::new(),
            skills: Vec::new(),
            input_types: Vec::new(),
            output_types: Vec::new(),
            tool_refs: Vec::new(),
            workflow_refs: Vec::new(),
            dependency_refs: Vec::new(),
            optimization_of_refs: Vec::new(),
            constraints: Vec::new(),
            tags: Vec::new(),
        }
    }

    pub fn with_namespace(mut self, namespace: CapabilityNamespace) -> Self {
        self.namespace = namespace;
        self
    }

    pub fn with_visibility(mut self, visibility: CapabilityVisibility) -> Self {
        self.visibility = visibility;
        self
    }

    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = description.into();
        self
    }

    pub fn with_tier(mut self, tier: CapabilityTier) -> Self {
        self.tier = tier;
        self
    }

    pub fn with_model_dependence(mut self, model_dependence: ModelDependence) -> Self {
        self.model_dependence = model_dependence;
        self
    }

    pub fn with_domain(mut self, domain: impl Into<String>) -> Self {
        self.domains.push(domain.into());
        self
    }

    pub fn with_skill(mut self, skill: impl Into<String>) -> Self {
        self.skills.push(skill.into());
        self
    }

    pub fn with_input_type(mut self, input_type: CapabilityInputType) -> Self {
        self.input_types.push(input_type);
        self
    }

    pub fn with_output_type(mut self, output_type: CapabilityOutputType) -> Self {
        self.output_types.push(output_type);
        self
    }

    pub fn with_tool_ref(mut self, tool_ref: impl Into<String>) -> Self {
        self.tool_refs.push(tool_ref.into());
        self
    }

    pub fn with_workflow_ref(mut self, workflow_ref: impl Into<String>) -> Self {
        self.workflow_refs.push(workflow_ref.into());
        self
    }

    pub fn with_dependency_ref(mut self, dependency_ref: impl Into<String>) -> Self {
        self.dependency_refs.push(dependency_ref.into());
        self
    }

    pub fn with_optimization_of_ref(mut self, capability_ref: impl Into<String>) -> Self {
        self.optimization_of_refs.push(capability_ref.into());
        self
    }

    pub fn with_constraint(mut self, constraint: impl Into<String>) -> Self {
        self.constraints.push(constraint.into());
        self
    }

    pub fn with_tag(mut self, tag: impl Into<String>) -> Self {
        self.tags.push(tag.into());
        self
    }

    pub fn is_visible_to(&self, namespace: &CapabilityNamespace) -> bool {
        match self.visibility {
            CapabilityVisibility::Private => self.namespace == *namespace,
            CapabilityVisibility::TenantShared => self.namespace.tenant_id == namespace.tenant_id,
            CapabilityVisibility::CrossTenantShared => true,
        }
    }

    pub fn is_shared_knowledge(&self) -> bool {
        self.tier == CapabilityTier::KnowledgeInterface
    }

    pub fn is_runtime_foundation(&self) -> bool {
        self.tier == CapabilityTier::RuntimeFoundation
    }

    pub fn is_atomic_unit(&self) -> bool {
        self.tier == CapabilityTier::AtomicUnit
    }

    pub fn is_fully_deterministic(&self) -> bool {
        self.model_dependence == ModelDependence::None
    }

    pub fn is_optimization_of_runtime_foundation(&self) -> bool {
        self.tier == CapabilityTier::AtomicUnit && !self.optimization_of_refs.is_empty()
    }
}

fn default_capability_visibility() -> CapabilityVisibility {
    CapabilityVisibility::Private
}

pub fn seed_capability_for_role(namespace: CapabilityNamespace, role: &str) -> CapabilityProfile {
    match role {
        "planner" => CapabilityProfile::new("capability.seed.planner", "Planning")
            .with_namespace(namespace)
            .with_visibility(CapabilityVisibility::TenantShared)
            .with_description("Breaks a task into steps and coordination decisions.")
            .with_tier(CapabilityTier::KnowledgeInterface)
            .with_model_dependence(ModelDependence::Required)
            .with_domain("planning")
            .with_skill("task_breakdown")
            .with_skill("coordination")
            .with_input_type(CapabilityInputType::StructuredTask)
            .with_output_type(CapabilityOutputType::TaskPlan)
            .with_output_type(CapabilityOutputType::Decision),
        "reviewer" => CapabilityProfile::new("capability.seed.reviewer", "Review")
            .with_namespace(namespace)
            .with_visibility(CapabilityVisibility::TenantShared)
            .with_description("Reviews outputs for gaps, risks, and regressions.")
            .with_tier(CapabilityTier::KnowledgeInterface)
            .with_model_dependence(ModelDependence::Required)
            .with_domain("review")
            .with_skill("risk_identification")
            .with_skill("quality_review")
            .with_input_type(CapabilityInputType::MarkdownDocument)
            .with_input_type(CapabilityInputType::SessionMessage)
            .with_output_type(CapabilityOutputType::ReviewNotes),
        "worker" => CapabilityProfile::new("capability.seed.worker", "Execution")
            .with_namespace(namespace)
            .with_visibility(CapabilityVisibility::TenantShared)
            .with_description("Executes the main work and produces direct task output.")
            .with_tier(CapabilityTier::KnowledgeInterface)
            .with_model_dependence(ModelDependence::Required)
            .with_domain("execution")
            .with_skill("implementation")
            .with_skill("delivery")
            .with_input_type(CapabilityInputType::StructuredTask)
            .with_output_type(CapabilityOutputType::ChatReply)
            .with_output_type(CapabilityOutputType::Summary),
        other => CapabilityProfile::new(format!("capability.seed.{other}"), other)
            .with_namespace(namespace)
            .with_description(format!("Seed capability for role `{other}`."))
            .with_tier(CapabilityTier::KnowledgeInterface)
            .with_model_dependence(ModelDependence::Required)
            .with_domain(other)
            .with_input_type(CapabilityInputType::NaturalLanguage)
            .with_output_type(CapabilityOutputType::ChatReply),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct CapabilityFrontmatter {
    id: String,
    r#type: String,
    title: String,
    tenant_id: String,
    user_id: String,
    visibility: CapabilityVisibility,
    tier: CapabilityTier,
    model_dependence: ModelDependence,
    domains: Vec<String>,
    skills: Vec<String>,
    input_types: Vec<CapabilityInputType>,
    output_types: Vec<CapabilityOutputType>,
    tool_refs: Vec<String>,
    workflow_refs: Vec<String>,
    dependency_refs: Vec<String>,
    optimization_of_refs: Vec<String>,
    constraints: Vec<String>,
    tags: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct CapabilityRepository {
    store: WorkspaceStore,
    namespace: WorkspaceNamespace,
}

impl CapabilityRepository {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self::with_namespace(root, WorkspaceNamespace::local_default())
    }

    pub fn with_namespace(root: impl Into<PathBuf>, namespace: WorkspaceNamespace) -> Self {
        Self {
            store: WorkspaceStore::new(root),
            namespace,
        }
    }

    pub fn relative_path_for(profile: &CapabilityProfile) -> PathBuf {
        PathBuf::from("capabilities").join(format!("{}.md", profile.id))
    }

    pub fn write_profile(&self, profile: &CapabilityProfile) -> Result<PathBuf> {
        let frontmatter = CapabilityFrontmatter::from_profile(profile, &self.namespace);
        let body = render_capability_body(profile);
        self.store.write_markdown_in_namespace(
            &self.namespace,
            Self::relative_path_for(profile),
            &frontmatter,
            &body,
        )
    }

    pub fn read_profile(&self, relative_path: impl AsRef<Path>) -> Result<CapabilityProfile> {
        let stored: StoredMarkdown<CapabilityFrontmatter> = self
            .store
            .read_markdown_in_namespace(&self.namespace, relative_path)?;
        Ok(CapabilityProfile::from_document(
            stored.frontmatter,
            stored.body,
        ))
    }
}

impl CapabilityProfile {
    fn from_document(frontmatter: CapabilityFrontmatter, body: String) -> Self {
        Self {
            id: frontmatter.id,
            namespace: CapabilityNamespace::new(frontmatter.tenant_id, frontmatter.user_id),
            visibility: frontmatter.visibility,
            name: frontmatter.title,
            description: extract_capability_description(&body),
            tier: frontmatter.tier,
            model_dependence: frontmatter.model_dependence,
            domains: frontmatter.domains,
            skills: frontmatter.skills,
            input_types: frontmatter.input_types,
            output_types: frontmatter.output_types,
            tool_refs: frontmatter.tool_refs,
            workflow_refs: frontmatter.workflow_refs,
            dependency_refs: frontmatter.dependency_refs,
            optimization_of_refs: frontmatter.optimization_of_refs,
            constraints: frontmatter.constraints,
            tags: frontmatter.tags,
        }
    }
}

impl CapabilityFrontmatter {
    fn from_profile(profile: &CapabilityProfile, namespace: &WorkspaceNamespace) -> Self {
        Self {
            id: profile.id.clone(),
            r#type: "capability".to_owned(),
            title: profile.name.clone(),
            tenant_id: if profile.namespace == CapabilityNamespace::local_default() {
                namespace.tenant_id.clone()
            } else {
                profile.namespace.tenant_id.clone()
            },
            user_id: if profile.namespace == CapabilityNamespace::local_default() {
                namespace.user_id.clone()
            } else {
                profile.namespace.user_id.clone()
            },
            visibility: profile.visibility.clone(),
            tier: profile.tier.clone(),
            model_dependence: profile.model_dependence.clone(),
            domains: profile.domains.clone(),
            skills: profile.skills.clone(),
            input_types: profile.input_types.clone(),
            output_types: profile.output_types.clone(),
            tool_refs: profile.tool_refs.clone(),
            workflow_refs: profile.workflow_refs.clone(),
            dependency_refs: profile.dependency_refs.clone(),
            optimization_of_refs: profile.optimization_of_refs.clone(),
            constraints: profile.constraints.clone(),
            tags: profile.tags.clone(),
        }
    }
}

fn render_capability_body(profile: &CapabilityProfile) -> String {
    let mut body = format!(
        "# {}\n\n{}\n\n## Positioning\n\n- tier: {}\n- model_dependence: {}\n",
        profile.name,
        profile.description,
        render_tier(&profile.tier),
        render_model_dependence(&profile.model_dependence)
    );

    if !profile.dependency_refs.is_empty() {
        body.push_str("\n## Dependencies\n\n");
        for dependency in &profile.dependency_refs {
            body.push_str(&format!("- {}\n", dependency));
        }
    }

    if !profile.optimization_of_refs.is_empty() {
        body.push_str("\n## Optimizes\n\n");
        for capability_ref in &profile.optimization_of_refs {
            body.push_str(&format!("- {}\n", capability_ref));
        }
    }

    if !profile.constraints.is_empty() {
        body.push_str("\n## Constraints\n\n");
        for constraint in &profile.constraints {
            body.push_str(&format!("- {}\n", constraint));
        }
    }

    body
}

fn extract_capability_description(body: &str) -> String {
    body.lines()
        .skip_while(|line| line.starts_with('#') || line.trim().is_empty())
        .take_while(|line| {
            !line.trim_start().starts_with("## Positioning")
                && !line.trim_start().starts_with("## Dependencies")
                && !line.trim_start().starts_with("## Optimizes")
                && !line.trim_start().starts_with("## Constraints")
        })
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_owned()
}

fn render_tier(tier: &CapabilityTier) -> &'static str {
    match tier {
        CapabilityTier::KnowledgeInterface => "knowledge_interface",
        CapabilityTier::RuntimeFoundation => "runtime_foundation",
        CapabilityTier::AtomicUnit => "atomic_unit",
    }
}

fn render_model_dependence(model_dependence: &ModelDependence) -> &'static str {
    match model_dependence {
        ModelDependence::Required => "required",
        ModelDependence::Optional => "optional",
        ModelDependence::None => "none",
    }
}

#[cfg(test)]
#[path = "../tests/unit/lib.rs"]
mod tests;
