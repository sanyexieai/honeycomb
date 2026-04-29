//! Shared toolchain primitives for planning, binding, and execution.

use anyhow::{Context, Result, bail};
use hc_capability::ModelDependence;
use hc_store::store::{MarkdownQuery, StoredMarkdown, WorkspaceNamespace, WorkspaceStore};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolExecutionKind {
    Script,
    Workflow,
    Cli,
    Service,
    Builtin,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolStability {
    Experimental,
    Managed,
    Stable,
    Foundational,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolComposition {
    Atomic,
    Composite,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolSpec {
    pub id: String,
    pub name: String,
    pub description: String,
    pub execution_kind: ToolExecutionKind,
    pub composition: ToolComposition,
    pub stability: ToolStability,
    pub model_dependence: ModelDependence,
    pub default_command: Vec<String>,
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct ToolFrontmatter {
    id: String,
    r#type: String,
    title: String,
    tenant_id: String,
    user_id: String,
    execution_kind: ToolExecutionKind,
    composition: ToolComposition,
    stability: ToolStability,
    model_dependence: ModelDependence,
    default_command: Vec<String>,
    tags: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolExecutionPlan {
    pub tool_id: String,
    pub suggested_command: Vec<String>,
    pub guidance: Vec<String>,
    pub validation_steps: Vec<String>,
    pub recovery_steps: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolExecutionOutcome {
    pub tool_id: String,
    pub parent_tool_id: Option<String>,
    pub invoked_tool_ids: Vec<String>,
    pub goal: String,
    pub command: Vec<String>,
    pub success: bool,
    pub summary: String,
    pub observations: Vec<String>,
}

impl ToolExecutionOutcome {
    pub fn with_parent_tool_id(mut self, parent_tool_id: impl Into<String>) -> Self {
        self.parent_tool_id = Some(parent_tool_id.into());
        self
    }

    pub fn wrapped_by(mut self, wrapper_tool_id: impl Into<String>) -> Self {
        let wrapper_tool_id = wrapper_tool_id.into();
        if self.tool_id != wrapper_tool_id
            && !self
                .invoked_tool_ids
                .iter()
                .any(|tool_id| tool_id == &self.tool_id)
        {
            self.invoked_tool_ids.insert(0, self.tool_id.clone());
        }
        self.tool_id = wrapper_tool_id;
        self.parent_tool_id = None;
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EvaluationSignal {
    HumanConfirmed,
    HumanRejected,
    ExecutionSucceeded,
    ExecutionFailed,
    ValidationPassed,
    ValidationFailed,
    RepeatedReuse,
    SupersededByNewerAsset,
}

pub trait ToolExecutor {
    fn execute(&self, plan: &ToolExecutionPlan, goal: &str) -> Result<ToolExecutionOutcome>;
}

#[derive(Debug, Clone)]
pub struct CommandToolExecutor {
    pub working_dir: Option<PathBuf>,
    pub max_observation_lines: usize,
}

impl Default for CommandToolExecutor {
    fn default() -> Self {
        Self {
            working_dir: None,
            max_observation_lines: 40,
        }
    }
}

impl CommandToolExecutor {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_working_dir(mut self, working_dir: impl Into<PathBuf>) -> Self {
        self.working_dir = Some(working_dir.into());
        self
    }

    pub fn with_max_observation_lines(mut self, max_observation_lines: usize) -> Self {
        self.max_observation_lines = max_observation_lines;
        self
    }
}

impl ToolExecutor for CommandToolExecutor {
    fn execute(&self, plan: &ToolExecutionPlan, goal: &str) -> Result<ToolExecutionOutcome> {
        let program = plan
            .suggested_command
            .first()
            .context("tool execution plan has no command")?;
        let mut command = Command::new(program);
        command.args(plan.suggested_command.iter().skip(1));
        if let Some(working_dir) = &self.working_dir {
            command.current_dir(working_dir);
        }

        let output = command
            .output()
            .with_context(|| format!("failed to execute tool command: {program}"))?;
        let success = output.status.success();
        let observations =
            command_observations(&output.stdout, &output.stderr, self.max_observation_lines);
        let summary = match output.status.code() {
            Some(code) => format!("command exited with status {code}"),
            None => "command terminated by signal".to_owned(),
        };

        Ok(ToolExecutionOutcome {
            tool_id: plan.tool_id.clone(),
            parent_tool_id: None,
            invoked_tool_ids: Vec::new(),
            goal: goal.to_owned(),
            command: plan.suggested_command.clone(),
            success,
            summary,
            observations,
        })
    }
}

pub trait ToolProvider {
    fn list_tools(&self) -> Vec<ToolSpec>;

    fn get_tool(&self, tool_id: &str) -> Option<ToolSpec> {
        self.list_tools()
            .into_iter()
            .find(|tool| tool.id == tool_id)
    }
}

#[derive(Debug, Clone, Default)]
pub struct ToolCatalog {
    tools: BTreeMap<String, ToolSpec>,
}

impl ToolCatalog {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, tool: ToolSpec) -> Option<ToolSpec> {
        self.tools.insert(tool.id.clone(), tool)
    }

    pub fn register_many<I>(&mut self, tools: I)
    where
        I: IntoIterator<Item = ToolSpec>,
    {
        for tool in tools {
            self.register(tool);
        }
    }

    pub fn register_provider(&mut self, provider: &impl ToolProvider) {
        self.register_many(provider.list_tools());
    }

    pub fn get(&self, tool_id: &str) -> Option<&ToolSpec> {
        self.tools.get(tool_id)
    }

    pub fn contains(&self, tool_id: &str) -> bool {
        self.tools.contains_key(tool_id)
    }

    pub fn list(&self) -> Vec<&ToolSpec> {
        self.tools.values().collect()
    }

    pub fn into_tools(self) -> Vec<ToolSpec> {
        self.tools.into_values().collect()
    }
}

impl ToolProvider for ToolCatalog {
    fn list_tools(&self) -> Vec<ToolSpec> {
        self.tools.values().cloned().collect()
    }

    fn get_tool(&self, tool_id: &str) -> Option<ToolSpec> {
        self.tools.get(tool_id).cloned()
    }
}

#[derive(Debug, Clone)]
pub struct ToolRepository {
    store: WorkspaceStore,
    namespace: WorkspaceNamespace,
}

impl ToolRepository {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self::with_namespace(root, WorkspaceNamespace::local_default())
    }

    pub fn with_namespace(root: impl Into<PathBuf>, namespace: WorkspaceNamespace) -> Self {
        Self {
            store: WorkspaceStore::new(root),
            namespace,
        }
    }

    pub fn relative_path_for(tool: &ToolSpec) -> PathBuf {
        PathBuf::from("tools").join(format!("{}.md", tool.id))
    }

    pub fn write_tool(&self, tool: &ToolSpec) -> Result<PathBuf> {
        validate_tool_spec(tool)?;
        let frontmatter = ToolFrontmatter::from_tool(tool, &self.namespace);
        let body = render_tool_body(tool);
        let path = self.store.write_markdown_in_namespace(
            &self.namespace,
            Self::relative_path_for(tool),
            &frontmatter,
            &body,
        )?;
        let _ = self
            .store
            .rebuild_markdown_index_in_namespace(&self.namespace);
        Ok(path)
    }

    pub fn read_tool(&self, relative_path: impl AsRef<Path>) -> Result<ToolSpec> {
        let stored: StoredMarkdown<ToolFrontmatter> = self
            .store
            .read_markdown_in_namespace(&self.namespace, relative_path)?;
        ToolSpec::from_document(stored.frontmatter, stored.body)
    }

    pub fn list_tools(&self) -> Result<Vec<ToolSpec>> {
        let _ = self
            .store
            .rebuild_markdown_index_in_namespace(&self.namespace);
        let query = MarkdownQuery::default()
            .with_path_prefix("tools/")
            .with_limit(500);
        let entries = self
            .store
            .query_markdown_index_in_namespace(&self.namespace, &query)?;
        let mut tools = Vec::new();
        for entry in entries {
            tools.push(self.read_tool(entry.relative_path)?);
        }
        tools.sort_by(|left, right| left.id.cmp(&right.id));
        Ok(tools)
    }

    pub fn load_catalog(&self) -> Result<ToolCatalog> {
        let mut catalog = ToolCatalog::new();
        catalog.register_many(self.list_tools()?);
        Ok(catalog)
    }
}

pub fn seed_tool_rg() -> ToolSpec {
    ToolSpec {
        id: "tool.rg".to_owned(),
        name: "Ripgrep".to_owned(),
        description: "Searches workspace files and source text with ripgrep.".to_owned(),
        execution_kind: ToolExecutionKind::Cli,
        composition: ToolComposition::Atomic,
        stability: ToolStability::Stable,
        model_dependence: ModelDependence::Optional,
        default_command: vec!["rg".to_owned(), "-n".to_owned()],
        tags: vec!["tool".to_owned(), "rg".to_owned(), "search".to_owned()],
    }
}

pub fn seed_tool_cargo_test() -> ToolSpec {
    ToolSpec {
        id: "tool.cargo-test".to_owned(),
        name: "Cargo Test".to_owned(),
        description: "Runs Rust test suites with cargo test.".to_owned(),
        execution_kind: ToolExecutionKind::Cli,
        composition: ToolComposition::Atomic,
        stability: ToolStability::Stable,
        model_dependence: ModelDependence::Optional,
        default_command: vec!["cargo".to_owned(), "test".to_owned()],
        tags: vec!["tool".to_owned(), "cargo".to_owned(), "test".to_owned()],
    }
}

pub fn seed_tool_local_file_read() -> ToolSpec {
    ToolSpec {
        id: "tool.local-file.read".to_owned(),
        name: "Local File Read".to_owned(),
        description: "Reads UTF-8 content from an explicit local file path.".to_owned(),
        execution_kind: ToolExecutionKind::Builtin,
        composition: ToolComposition::Atomic,
        stability: ToolStability::Stable,
        model_dependence: ModelDependence::Optional,
        default_command: vec!["hc.local-file.read".to_owned()],
        tags: vec![
            "tool".to_owned(),
            "local-file".to_owned(),
            "read".to_owned(),
            "workspace".to_owned(),
        ],
    }
}

pub fn seed_tool_local_file_write() -> ToolSpec {
    ToolSpec {
        id: "tool.local-file.write".to_owned(),
        name: "Local File Write".to_owned(),
        description: "Writes UTF-8 content to an explicit local file path.".to_owned(),
        execution_kind: ToolExecutionKind::Builtin,
        composition: ToolComposition::Atomic,
        stability: ToolStability::Stable,
        model_dependence: ModelDependence::Optional,
        default_command: vec!["hc.local-file.write".to_owned()],
        tags: vec![
            "tool".to_owned(),
            "local-file".to_owned(),
            "write".to_owned(),
            "workspace".to_owned(),
        ],
    }
}

pub fn seed_tool_local_dir_list() -> ToolSpec {
    ToolSpec {
        id: "tool.local-dir.list".to_owned(),
        name: "Local Directory List".to_owned(),
        description: "Lists entries in an explicit local directory path.".to_owned(),
        execution_kind: ToolExecutionKind::Builtin,
        composition: ToolComposition::Atomic,
        stability: ToolStability::Stable,
        model_dependence: ModelDependence::Optional,
        default_command: vec!["hc.local-dir.list".to_owned()],
        tags: vec![
            "tool".to_owned(),
            "local-dir".to_owned(),
            "list".to_owned(),
            "workspace".to_owned(),
        ],
    }
}

pub fn default_tool_catalog() -> ToolCatalog {
    let mut catalog = ToolCatalog::new();
    catalog.register_many([
        seed_tool_rg(),
        seed_tool_cargo_test(),
        seed_tool_local_file_read(),
        seed_tool_local_file_write(),
        seed_tool_local_dir_list(),
    ]);
    catalog
}

pub fn validate_tool_spec(tool: &ToolSpec) -> Result<()> {
    if tool.id.trim().is_empty() {
        bail!("tool id cannot be empty");
    }
    if !tool.id.starts_with("tool.") {
        bail!("tool id must start with tool.");
    }
    if tool.name.trim().is_empty() {
        bail!("tool name cannot be empty");
    }
    if tool.description.trim().is_empty() {
        bail!("tool description cannot be empty");
    }
    if matches!(
        tool.execution_kind,
        ToolExecutionKind::Cli | ToolExecutionKind::Script
    ) && tool.default_command.is_empty()
    {
        bail!("{} requires a default command", tool.id);
    }
    Ok(())
}

pub fn builtin_tool(tool_id: &str) -> Option<ToolSpec> {
    default_tool_catalog().get_tool(tool_id)
}

pub fn default_tool_command(tool: &ToolSpec, goal: &str) -> Vec<String> {
    match tool.id.as_str() {
        "tool.rg" => default_rg_command(goal),
        "tool.cargo-test" => default_cargo_test_command(goal),
        "tool.local-file.read" => vec!["hc.local-file.read".to_owned()],
        "tool.local-file.write" => vec!["hc.local-file.write".to_owned()],
        "tool.local-dir.list" => vec!["hc.local-dir.list".to_owned()],
        _ => tool.default_command.clone(),
    }
}

pub fn build_default_tool_execution_plan(tool: &ToolSpec, goal: &str) -> Result<ToolExecutionPlan> {
    if tool.default_command.is_empty() {
        bail!("tool {} does not define a default command", tool.id);
    }

    Ok(ToolExecutionPlan {
        tool_id: tool.id.clone(),
        suggested_command: default_tool_command(tool, goal),
        guidance: default_tool_guidance(tool),
        validation_steps: default_tool_validation_steps(tool),
        recovery_steps: default_tool_recovery_steps(tool),
    })
}

fn default_rg_command(goal: &str) -> Vec<String> {
    let _ = goal;
    vec!["rg".to_owned(), "-n".to_owned()]
}

fn default_cargo_test_command(_goal: &str) -> Vec<String> {
    vec!["cargo".to_owned(), "test".to_owned()]
}

impl ToolSpec {
    fn from_document(frontmatter: ToolFrontmatter, body: String) -> Result<Self> {
        let description = split_tool_body(&body);
        let tool = Self {
            id: frontmatter.id,
            name: frontmatter.title,
            description,
            execution_kind: frontmatter.execution_kind,
            composition: frontmatter.composition,
            stability: frontmatter.stability,
            model_dependence: frontmatter.model_dependence,
            default_command: frontmatter.default_command,
            tags: frontmatter.tags,
        };
        validate_tool_spec(&tool)?;
        Ok(tool)
    }
}

impl ToolFrontmatter {
    fn from_tool(tool: &ToolSpec, namespace: &WorkspaceNamespace) -> Self {
        Self {
            id: tool.id.clone(),
            r#type: "tool".to_owned(),
            title: tool.name.clone(),
            tenant_id: namespace.tenant_id.clone(),
            user_id: namespace.user_id.clone(),
            execution_kind: tool.execution_kind.clone(),
            composition: tool.composition.clone(),
            stability: tool.stability.clone(),
            model_dependence: tool.model_dependence.clone(),
            default_command: tool.default_command.clone(),
            tags: tool.tags.clone(),
        }
    }
}

fn render_tool_body(tool: &ToolSpec) -> String {
    format!("# {}\n\n{}\n", tool.name, tool.description.trim())
}

fn split_tool_body(body: &str) -> String {
    let content = body.trim();
    if let Some(rest) = content.strip_prefix("# ") {
        rest.split_once('\n')
            .map(|(_, remaining)| remaining.trim().to_owned())
            .unwrap_or_default()
    } else {
        content.to_owned()
    }
}

fn default_tool_guidance(tool: &ToolSpec) -> Vec<String> {
    match tool.id.as_str() {
        "tool.rg" => vec![
            "Start with a narrow query and broaden only when results are empty.".to_owned(),
            "Prefer path filters when the goal names a crate, app, or document area.".to_owned(),
        ],
        "tool.cargo-test" => vec![
            "Run the smallest relevant test target first.".to_owned(),
            "Escalate to the full workspace only after focused checks pass.".to_owned(),
        ],
        "tool.local-file.read" => {
            vec!["Read only the explicit local path needed for the current goal.".to_owned()]
        }
        "tool.local-file.write" => {
            vec!["Write only to an explicit local path requested for the current goal.".to_owned()]
        }
        "tool.local-dir.list" => {
            vec!["List only the explicit local directory needed for the current goal.".to_owned()]
        }
        _ => vec![format!("Use {} for the requested goal.", tool.name)],
    }
}

fn default_tool_validation_steps(tool: &ToolSpec) -> Vec<String> {
    match tool.id.as_str() {
        "tool.rg" => vec!["Check whether the result set is specific enough to act on.".to_owned()],
        "tool.cargo-test" => vec!["Confirm cargo reports a successful test result.".to_owned()],
        "tool.local-file.read" => vec!["Confirm the read path and byte count.".to_owned()],
        "tool.local-file.write" => vec!["Confirm the written path and byte count.".to_owned()],
        "tool.local-dir.list" => vec!["Confirm the listed path and entry count.".to_owned()],
        _ => {
            vec!["Review the command output before treating the outcome as successful.".to_owned()]
        }
    }
}

fn default_tool_recovery_steps(tool: &ToolSpec) -> Vec<String> {
    match tool.id.as_str() {
        "tool.rg" => vec![
            "Retry with a simpler token if there are no matches.".to_owned(),
            "Use file-list mode when the goal is about locating files.".to_owned(),
        ],
        "tool.cargo-test" => vec![
            "Retry with a package or test filter if the full run is too broad.".to_owned(),
            "Inspect the first failing test before rerunning wider suites.".to_owned(),
        ],
        "tool.local-file.read" => vec![
            "Retry with a project-relative or absolute path if the file is not found.".to_owned(),
        ],
        "tool.local-file.write" => {
            vec!["Create missing parent directories before retrying the write.".to_owned()]
        }
        "tool.local-dir.list" => {
            vec!["Retry with a project-relative or absolute directory path.".to_owned()]
        }
        _ => vec!["Adjust arguments and rerun the tool if output is not actionable.".to_owned()],
    }
}

fn command_observations(stdout: &[u8], stderr: &[u8], max_lines: usize) -> Vec<String> {
    let mut observations = Vec::new();
    push_prefixed_lines(&mut observations, "stdout", stdout, max_lines);
    if observations.len() < max_lines {
        push_prefixed_lines(&mut observations, "stderr", stderr, max_lines);
    }
    observations
}

fn push_prefixed_lines(
    observations: &mut Vec<String>,
    prefix: &str,
    bytes: &[u8],
    max_lines: usize,
) {
    let text = String::from_utf8_lossy(bytes);
    for line in text.lines() {
        if observations.len() >= max_lines {
            break;
        }
        observations.push(format!("{prefix}: {line}"));
    }
}

#[cfg(test)]
#[path = "../tests/unit/lib.rs"]
mod tests;
