//! Shared toolchain primitives for planning, binding, and execution.

use anyhow::{Context, Result, bail};
use hc_capability::ModelDependence;
use hc_store::store::{MarkdownQuery, StoredMarkdown, WorkspaceNamespace, WorkspaceStore};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const DEFAULT_MCP_REQUEST_TIMEOUT: Duration = Duration::from_secs(8);

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
#[serde(rename_all = "snake_case")]
pub enum McpTransportKind {
    Stdio,
    StreamableHttp,
    Sse,
}

fn default_mcp_transport_kind() -> McpTransportKind {
    McpTransportKind::Stdio
}

fn default_enabled() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct McpServerSpec {
    pub id: String,
    pub name: String,
    pub description: String,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(default = "default_mcp_transport_kind")]
    pub transport: McpTransportKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default)]
    pub command: Vec<String>,
    #[serde(default)]
    pub default_args: BTreeMap<String, Value>,
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
struct McpServerFrontmatter {
    id: String,
    r#type: String,
    title: String,
    tenant_id: String,
    user_id: String,
    #[serde(default = "default_enabled")]
    enabled: bool,
    #[serde(default = "default_mcp_transport_kind")]
    transport: McpTransportKind,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    command: Vec<String>,
    #[serde(default)]
    default_args: BTreeMap<String, Value>,
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

#[derive(Debug, Clone)]
pub struct McpServerRepository {
    store: WorkspaceStore,
    namespace: WorkspaceNamespace,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct McpToolCache {
    pub server_id: String,
    pub refreshed_at_unix: u64,
    pub tools: Vec<ToolSpec>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct McpToolCacheFrontmatter {
    id: String,
    r#type: String,
    title: String,
    tenant_id: String,
    user_id: String,
    server_id: String,
    refreshed_at_unix: u64,
    tools: Vec<ToolSpec>,
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

impl McpServerRepository {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self::with_namespace(root, WorkspaceNamespace::local_default())
    }

    pub fn with_namespace(root: impl Into<PathBuf>, namespace: WorkspaceNamespace) -> Self {
        Self {
            store: WorkspaceStore::new(root),
            namespace,
        }
    }

    pub fn relative_path_for(server: &McpServerSpec) -> PathBuf {
        PathBuf::from("mcp")
            .join("servers")
            .join(format!("{}.md", server.id))
    }

    pub fn write_server(&self, server: &McpServerSpec) -> Result<PathBuf> {
        validate_mcp_server_spec(server)?;
        let frontmatter = McpServerFrontmatter::from_server(server, &self.namespace);
        let body = render_mcp_server_body(server);
        let path = self.store.write_markdown_in_namespace(
            &self.namespace,
            Self::relative_path_for(server),
            &frontmatter,
            &body,
        )?;
        let _ = self
            .store
            .rebuild_markdown_index_in_namespace(&self.namespace);
        Ok(path)
    }

    pub fn read_server(&self, relative_path: impl AsRef<Path>) -> Result<McpServerSpec> {
        let stored: StoredMarkdown<McpServerFrontmatter> = self
            .store
            .read_markdown_in_namespace(&self.namespace, relative_path)?;
        McpServerSpec::from_document(stored.frontmatter, stored.body)
    }

    pub fn list_servers(&self) -> Result<Vec<McpServerSpec>> {
        let _ = self
            .store
            .rebuild_markdown_index_in_namespace(&self.namespace);
        let query = MarkdownQuery::default()
            .with_path_prefix("mcp/servers/")
            .with_limit(200);
        let entries = self
            .store
            .query_markdown_index_in_namespace(&self.namespace, &query)?;
        let mut servers = Vec::new();
        for entry in entries {
            servers.push(self.read_server(entry.relative_path)?);
        }
        servers.sort_by(|left, right| left.id.cmp(&right.id));
        Ok(servers)
    }

    pub fn get_server(&self, server_id: &str) -> Result<McpServerSpec> {
        let normalized = normalize_mcp_server_id(server_id);
        self.list_servers()?
            .into_iter()
            .find(|server| server.id == normalized)
            .with_context(|| format!("unknown mcp server: {normalized}"))
    }

    pub fn cache_relative_path_for(server_id: &str) -> PathBuf {
        PathBuf::from("mcp")
            .join("cache")
            .join(format!("{}.tools.md", normalize_mcp_server_id(server_id)))
    }

    pub fn read_tool_cache(&self, server_id: &str) -> Result<McpToolCache> {
        let stored: StoredMarkdown<McpToolCacheFrontmatter> =
            self.store.read_markdown_in_namespace(
                &self.namespace,
                Self::cache_relative_path_for(server_id),
            )?;
        Ok(stored.frontmatter.into_cache())
    }

    pub fn quarantine_tool_cache(&self, server_id: &str, reason: &str) -> Result<Option<PathBuf>> {
        let source_relative = Self::cache_relative_path_for(server_id);
        let source = self
            .store
            .resolve_in_namespace(&self.namespace, &source_relative);
        if !source.exists() {
            return Ok(None);
        }
        let target_relative = PathBuf::from("mcp").join("cache").join("bad").join(format!(
            "{}.{}.broken.md",
            normalize_mcp_server_id(server_id),
            current_unix_timestamp()
        ));
        let target = self
            .store
            .resolve_in_namespace(&self.namespace, &target_relative);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!(
                    "failed to create MCP bad cache directory {}",
                    parent.display()
                )
            })?;
        }
        std::fs::rename(&source, &target).with_context(|| {
            format!(
                "failed to quarantine MCP tool cache {} to {} after {reason}",
                source.display(),
                target.display()
            )
        })?;
        let _ = self
            .store
            .rebuild_markdown_index_in_namespace(&self.namespace);
        Ok(Some(target_relative))
    }

    pub fn write_tool_cache(&self, server_id: &str, tools: Vec<ToolSpec>) -> Result<PathBuf> {
        let cache = McpToolCache {
            server_id: normalize_mcp_server_id(server_id),
            refreshed_at_unix: current_unix_timestamp(),
            tools,
        };
        let frontmatter = McpToolCacheFrontmatter::from_cache(&cache, &self.namespace);
        let body = render_mcp_tool_cache_body(&cache);
        let path = self.store.write_markdown_in_namespace(
            &self.namespace,
            Self::cache_relative_path_for(server_id),
            &frontmatter,
            &body,
        )?;
        let _ = self
            .store
            .rebuild_markdown_index_in_namespace(&self.namespace);
        Ok(path)
    }

    pub fn refresh_tool_cache(&self, server: &McpServerSpec) -> Result<McpToolCache> {
        let tools = discover_mcp_tools(server)?;
        self.write_tool_cache(&server.id, tools.clone())?;
        Ok(McpToolCache {
            server_id: server.id.clone(),
            refreshed_at_unix: current_unix_timestamp(),
            tools,
        })
    }
}

impl McpToolCacheFrontmatter {
    fn from_cache(cache: &McpToolCache, namespace: &WorkspaceNamespace) -> Self {
        Self {
            id: format!("cache.{}.tools", cache.server_id),
            r#type: "mcp_tool_cache".to_owned(),
            title: format!("{} Tool Cache", cache.server_id),
            tenant_id: namespace.tenant_id.clone(),
            user_id: namespace.user_id.clone(),
            server_id: cache.server_id.clone(),
            refreshed_at_unix: cache.refreshed_at_unix,
            tools: cache.tools.clone(),
        }
    }

    fn into_cache(self) -> McpToolCache {
        McpToolCache {
            server_id: self.server_id,
            refreshed_at_unix: self.refreshed_at_unix,
            tools: self.tools,
        }
    }
}

fn render_mcp_tool_cache_body(cache: &McpToolCache) -> String {
    let mut lines = vec![
        format!("# {} Tool Cache", cache.server_id),
        String::new(),
        format!("Refreshed at unix timestamp: {}", cache.refreshed_at_unix),
        String::new(),
    ];
    for tool in &cache.tools {
        lines.push(format!("- `{}`: {}", tool.id, tool.description));
    }
    lines.join("\n")
}

fn current_unix_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
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

pub fn validate_mcp_server_spec(server: &McpServerSpec) -> Result<()> {
    if server.id.trim().is_empty() {
        bail!("mcp server id cannot be empty");
    }
    if !server.id.starts_with("mcp.") {
        bail!("mcp server id must start with mcp.");
    }
    if server.name.trim().is_empty() {
        bail!("mcp server name cannot be empty");
    }
    match server.transport {
        McpTransportKind::Stdio => {
            if server.command.is_empty() {
                bail!("mcp stdio server {} does not define a command", server.id);
            }
        }
        McpTransportKind::StreamableHttp | McpTransportKind::Sse => {
            if server.url.as_deref().unwrap_or_default().trim().is_empty() {
                bail!("mcp http server {} does not define a url", server.id);
            }
        }
    }
    Ok(())
}

pub fn normalize_mcp_server_id(value: &str) -> String {
    if value.starts_with("mcp.") {
        value.to_owned()
    } else {
        format!("mcp.{value}")
    }
}

pub fn mcp_tool_id(server_id: &str, tool_name: &str) -> String {
    format!(
        "tool.mcp.{}.{}",
        slugify_tool_segment(server_id.trim_start_matches("mcp.")),
        slugify_tool_segment(tool_name)
    )
}

pub fn is_mcp_tool_command(command: &[String]) -> bool {
    command.first().is_some_and(|token| token == "hc.mcp.call") && command.len() >= 3
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

impl McpServerSpec {
    fn from_document(frontmatter: McpServerFrontmatter, body: String) -> Result<Self> {
        let description = split_tool_body(&body);
        let server = Self {
            id: frontmatter.id,
            name: frontmatter.title,
            description,
            enabled: frontmatter.enabled,
            transport: frontmatter.transport,
            url: frontmatter.url,
            command: frontmatter.command,
            default_args: frontmatter.default_args,
            tags: frontmatter.tags,
        };
        validate_mcp_server_spec(&server)?;
        Ok(server)
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

impl McpServerFrontmatter {
    fn from_server(server: &McpServerSpec, namespace: &WorkspaceNamespace) -> Self {
        Self {
            id: server.id.clone(),
            r#type: "mcp_server".to_owned(),
            title: server.name.clone(),
            tenant_id: namespace.tenant_id.clone(),
            user_id: namespace.user_id.clone(),
            enabled: server.enabled,
            transport: server.transport.clone(),
            url: server.url.clone(),
            command: server.command.clone(),
            default_args: server.default_args.clone(),
            tags: server.tags.clone(),
        }
    }
}

fn render_tool_body(tool: &ToolSpec) -> String {
    format!("# {}\n\n{}\n", tool.name, tool.description.trim())
}

fn render_mcp_server_body(server: &McpServerSpec) -> String {
    format!("# {}\n\n{}\n", server.name, server.description.trim())
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

pub fn discover_mcp_tools(server: &McpServerSpec) -> Result<Vec<ToolSpec>> {
    discover_mcp_tools_with_timeout(server, DEFAULT_MCP_REQUEST_TIMEOUT)
}

pub fn discover_mcp_tools_with_timeout(
    server: &McpServerSpec,
    timeout: Duration,
) -> Result<Vec<ToolSpec>> {
    let mut session = McpSession::start(server, timeout)?;
    session.initialize()?;
    let response = session.request("tools/list", json!({}))?;
    let tools = response
        .get("tools")
        .and_then(Value::as_array)
        .context("mcp tools/list response missed tools array")?;
    let mut specs = Vec::new();
    for tool in tools {
        let Some(name) = tool.get("name").and_then(Value::as_str) else {
            continue;
        };
        let description = tool
            .get("description")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .unwrap_or("MCP tool exposed by a configured server.");
        let mut tags = vec![
            "tool".to_owned(),
            "mcp".to_owned(),
            server.id.clone(),
            slugify_tool_segment(name),
        ];
        tags.extend(server.tags.iter().cloned());
        tags.sort();
        tags.dedup();
        specs.push(ToolSpec {
            id: mcp_tool_id(&server.id, name),
            name: format!("{} / {}", server.name, name),
            description: description.to_owned(),
            execution_kind: ToolExecutionKind::Service,
            composition: ToolComposition::Atomic,
            stability: ToolStability::Managed,
            model_dependence: ModelDependence::Optional,
            default_command: vec!["hc.mcp.call".to_owned(), server.id.clone(), name.to_owned()],
            tags,
        });
    }
    specs.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(specs)
}

pub fn call_mcp_tool(server: &McpServerSpec, tool_name: &str, arguments: Value) -> Result<Value> {
    call_mcp_tool_with_timeout(server, tool_name, arguments, DEFAULT_MCP_REQUEST_TIMEOUT)
}

pub fn call_mcp_tool_with_timeout(
    server: &McpServerSpec,
    tool_name: &str,
    arguments: Value,
    timeout: Duration,
) -> Result<Value> {
    let mut session = McpSession::start(server, timeout)?;
    session.initialize()?;
    session.request(
        "tools/call",
        json!({
            "name": tool_name,
            "arguments": arguments,
        }),
    )
}

enum McpSession {
    Stdio(McpStdioSession),
    Http(McpHttpSession),
}

impl McpSession {
    fn start(server: &McpServerSpec, timeout: Duration) -> Result<Self> {
        match server.transport {
            McpTransportKind::Stdio => Ok(Self::Stdio(McpStdioSession::start(server, timeout)?)),
            McpTransportKind::StreamableHttp | McpTransportKind::Sse => {
                Ok(Self::Http(McpHttpSession::start(server, timeout)?))
            }
        }
    }

    fn initialize(&mut self) -> Result<()> {
        match self {
            Self::Stdio(session) => session.initialize(),
            Self::Http(session) => session.initialize(),
        }
    }

    fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        match self {
            Self::Stdio(session) => session.request(method, params),
            Self::Http(session) => session.request(method, params),
        }
    }
}

struct McpHttpSession {
    client: reqwest::blocking::Client,
    url: String,
    post_url: String,
    transport: McpTransportKind,
    next_id: u64,
    protocol_version: String,
    session_id: Option<String>,
    sse_reader: Option<BufReader<reqwest::blocking::Response>>,
}

impl McpHttpSession {
    fn start(server: &McpServerSpec, timeout: Duration) -> Result<Self> {
        validate_mcp_server_spec(server)?;
        let url = server
            .url
            .as_deref()
            .context("mcp http server missed url")?
            .trim()
            .to_owned();
        let client = reqwest::blocking::Client::builder()
            .timeout(timeout)
            .build()
            .context("failed to build mcp http client")?;
        let mut session = Self {
            client,
            post_url: url.clone(),
            url: url.clone(),
            transport: server.transport.clone(),
            next_id: 1,
            protocol_version: "2025-06-18".to_owned(),
            session_id: None,
            sse_reader: None,
        };
        if server.transport == McpTransportKind::Sse {
            session.connect_sse()?;
        }
        Ok(session)
    }

    fn initialize(&mut self) -> Result<()> {
        self.request(
            "initialize",
            json!({
                "protocolVersion": self.protocol_version,
                "capabilities": {},
                "clientInfo": {
                    "name": "honeycomb",
                    "version": env!("CARGO_PKG_VERSION")
                }
            }),
        )?;
        self.notify("notifications/initialized", json!({}))?;
        Ok(())
    }

    fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id;
        self.next_id += 1;
        let message = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        let response = self.post_message(&message, Some(id))?;
        if let Some(error) = response.get("error") {
            bail!("mcp {method} failed: {error}");
        }
        Ok(response.get("result").cloned().unwrap_or(Value::Null))
    }

    fn notify(&mut self, method: &str, params: Value) -> Result<()> {
        let message = json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });
        let _ = self.post_message(&message, None)?;
        Ok(())
    }

    fn connect_sse(&mut self) -> Result<()> {
        let response = self
            .client
            .get(&self.url)
            .header("Accept", "text/event-stream")
            .send()
            .with_context(|| format!("failed to open mcp sse stream {}", self.url))?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().unwrap_or_default();
            bail!("mcp sse connect failed with {status}: {body}");
        }

        let mut reader = BufReader::new(response);
        let endpoint = loop {
            let event = read_sse_event(&mut reader).context("failed to read mcp sse endpoint")?;
            if event.event.as_deref() == Some("endpoint") && !event.data.trim().is_empty() {
                break event.data.trim().to_owned();
            }
        };
        self.post_url = resolve_mcp_sse_endpoint(&self.url, &endpoint)?;
        self.sse_reader = Some(reader);
        Ok(())
    }

    fn post_message(&mut self, message: &Value, request_id: Option<u64>) -> Result<Value> {
        let mut request = self
            .client
            .post(&self.post_url)
            .header("Accept", "application/json, text/event-stream")
            .header("Content-Type", "application/json")
            .header("MCP-Protocol-Version", &self.protocol_version)
            .json(message);
        if let Some(session_id) = &self.session_id {
            request = request.header("Mcp-Session-Id", session_id);
        }
        let response = request
            .send()
            .with_context(|| format!("failed to post mcp http message to {}", self.post_url))?;
        if let Some(session_id) = response.headers().get("Mcp-Session-Id") {
            self.session_id = Some(
                session_id
                    .to_str()
                    .context("invalid Mcp-Session-Id header")?
                    .to_owned(),
            );
        }
        let status = response.status();
        if !status.is_success() {
            let body = response.text().unwrap_or_default();
            bail!("mcp http request failed with {status}: {body}");
        }
        if self.transport == McpTransportKind::Sse {
            return match request_id {
                Some(id) => self.read_sse_response(id),
                None => Ok(Value::Null),
            };
        }
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_owned();
        let body = response.text().context("failed to read mcp http body")?;
        parse_mcp_http_response_body(&content_type, &body)
    }

    fn read_sse_response(&mut self, request_id: u64) -> Result<Value> {
        let reader = self
            .sse_reader
            .as_mut()
            .context("mcp sse session is not connected")?;
        loop {
            let event = read_sse_event(reader).context("failed to read mcp sse response")?;
            if event.data.trim().is_empty() {
                continue;
            }
            let value: Value =
                serde_json::from_str(&event.data).context("failed to parse mcp sse message")?;
            if value.get("id").and_then(Value::as_u64) == Some(request_id) {
                return Ok(value);
            }
        }
    }
}

struct SseEvent {
    event: Option<String>,
    data: String,
}

fn read_sse_event<R: BufRead>(reader: &mut R) -> Result<SseEvent> {
    let mut event = None;
    let mut data = Vec::new();
    loop {
        let mut line = String::new();
        let read = reader
            .read_line(&mut line)
            .context("failed to read sse line")?;
        if read == 0 {
            bail!("mcp sse stream closed");
        }
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            if event.is_some() || !data.is_empty() {
                return Ok(SseEvent {
                    event,
                    data: data.join("\n"),
                });
            }
            continue;
        }
        if line.starts_with(':') {
            continue;
        }
        if let Some(value) = line.strip_prefix("event:") {
            event = Some(value.trim().to_owned());
        } else if let Some(value) = line.strip_prefix("data:") {
            data.push(value.trim().to_owned());
        }
    }
}

fn resolve_mcp_sse_endpoint(base_url: &str, endpoint: &str) -> Result<String> {
    if endpoint.starts_with("http://") || endpoint.starts_with("https://") {
        return Ok(endpoint.to_owned());
    }
    let base = reqwest::Url::parse(base_url).context("invalid mcp sse url")?;
    if endpoint.starts_with('/') {
        let origin = format!(
            "{}://{}",
            base.scheme(),
            base.host_str().unwrap_or_default()
        );
        let origin = if let Some(port) = base.port() {
            format!("{origin}:{port}")
        } else {
            origin
        };
        return Ok(format!("{origin}{endpoint}"));
    }
    Ok(base
        .join(endpoint)
        .with_context(|| format!("invalid mcp sse endpoint: {endpoint}"))?
        .to_string())
}

fn parse_mcp_http_response_body(content_type: &str, body: &str) -> Result<Value> {
    if content_type.contains("text/event-stream") {
        let data = body
            .lines()
            .filter_map(|line| line.strip_prefix("data:"))
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .collect::<Vec<_>>()
            .join("\n");
        if data.trim().is_empty() {
            return Ok(Value::Null);
        }
        return serde_json::from_str(&data).context("failed to parse mcp sse data");
    }
    if body.trim().is_empty() {
        return Ok(Value::Null);
    }
    serde_json::from_str(body).context("failed to parse mcp http json response")
}

struct McpStdioSession {
    child: Child,
    stdin: ChildStdin,
    messages: mpsc::Receiver<Result<Value, String>>,
    next_id: u64,
    timeout: Duration,
}

impl McpStdioSession {
    fn start(server: &McpServerSpec, timeout: Duration) -> Result<Self> {
        validate_mcp_server_spec(server)?;
        let program = server
            .command
            .first()
            .context("mcp server command is empty")?;
        let mut command = Command::new(program);
        command.args(server.command.iter().skip(1));
        command.stdin(Stdio::piped());
        command.stdout(Stdio::piped());
        command.stderr(Stdio::null());
        let mut child = command
            .spawn()
            .with_context(|| format!("failed to start mcp server command: {program}"))?;
        let stdin = child
            .stdin
            .take()
            .context("failed to open mcp server stdin")?;
        let stdout = child
            .stdout
            .take()
            .context("failed to open mcp server stdout")?;
        let messages = spawn_mcp_stdout_reader(stdout);
        Ok(Self {
            child,
            stdin,
            messages,
            next_id: 1,
            timeout,
        })
    }

    fn initialize(&mut self) -> Result<()> {
        self.request(
            "initialize",
            json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {
                    "name": "honeycomb",
                    "version": env!("CARGO_PKG_VERSION")
                }
            }),
        )?;
        self.notify("notifications/initialized", json!({}))?;
        Ok(())
    }

    fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id;
        self.next_id += 1;
        self.write_message(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        }))?;

        loop {
            let message = self.read_message()?;
            if message.get("id").and_then(Value::as_u64) != Some(id) {
                continue;
            }
            if let Some(error) = message.get("error") {
                bail!("mcp {method} failed: {error}");
            }
            return Ok(message.get("result").cloned().unwrap_or(Value::Null));
        }
    }

    fn notify(&mut self, method: &str, params: Value) -> Result<()> {
        self.write_message(&json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        }))
    }

    fn write_message(&mut self, message: &Value) -> Result<()> {
        let body = serde_json::to_vec(message).context("failed to serialize mcp message")?;
        write!(self.stdin, "Content-Length: {}\r\n\r\n", body.len())
            .context("failed to write mcp header")?;
        self.stdin
            .write_all(&body)
            .context("failed to write mcp body")?;
        self.stdin.flush().context("failed to flush mcp stdin")?;
        Ok(())
    }

    fn read_message(&mut self) -> Result<Value> {
        match self.messages.recv_timeout(self.timeout) {
            Ok(Ok(message)) => Ok(message),
            Ok(Err(message)) => bail!("{message}"),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                bail!("mcp request timed out after {:?}", self.timeout)
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                bail!("mcp server stdout reader stopped")
            }
        }
    }
}

fn spawn_mcp_stdout_reader(stdout: ChildStdout) -> mpsc::Receiver<Result<Value, String>> {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let mut stdout = BufReader::new(stdout);
        loop {
            let message =
                read_mcp_message_from_stdout(&mut stdout).map_err(|error| error.to_string());
            let should_continue = message.is_ok();
            if sender.send(message).is_err() || !should_continue {
                break;
            }
        }
    });
    receiver
}

fn read_mcp_message_from_stdout(stdout: &mut BufReader<ChildStdout>) -> Result<Value> {
    let mut content_length = None;
    loop {
        let mut line = String::new();
        let read = stdout
            .read_line(&mut line)
            .context("failed to read mcp header")?;
        if read == 0 {
            bail!("mcp server closed stdout while reading headers");
        }
        let header = line.trim_end_matches(['\r', '\n']);
        if header.is_empty() {
            break;
        }
        if let Some(value) = header.strip_prefix("Content-Length:") {
            content_length = Some(
                value
                    .trim()
                    .parse::<usize>()
                    .context("invalid mcp content length")?,
            );
        }
    }
    let content_length = content_length.context("mcp message missed Content-Length header")?;
    let mut body = vec![0; content_length];
    stdout
        .read_exact(&mut body)
        .context("failed to read mcp body")?;
    serde_json::from_slice(&body).context("failed to parse mcp message")
}

impl Drop for McpStdioSession {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn slugify_tool_segment(value: &str) -> String {
    let mut out = String::new();
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    let trimmed = out.trim_matches('-').to_owned();
    if trimmed.is_empty() {
        "tool".to_owned()
    } else {
        trimmed
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
