use hc_responder::ResponderKind;
use hc_store::store::WorkspaceNamespace;
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActivityItemView {
    pub kind: String,
    pub stage: String,
    pub actor: String,
    pub title: String,
    pub detail: String,
}

impl ActivityItemView {
    pub fn new(
        kind: impl Into<String>,
        stage: impl Into<String>,
        actor: impl Into<String>,
        title: impl Into<String>,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            kind: kind.into(),
            stage: stage.into(),
            actor: actor.into(),
            title: title.into(),
            detail: detail.into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DecisionTraceView {
    pub code: String,
    pub stage: String,
    pub subject: String,
    pub outcome: String,
    pub detail: String,
}

impl DecisionTraceView {
    pub fn new(
        code: impl Into<String>,
        stage: impl Into<String>,
        subject: impl Into<String>,
        outcome: impl Into<String>,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            code: code.into(),
            stage: stage.into(),
            subject: subject.into(),
            outcome: outcome.into(),
            detail: detail.into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TraceLevel {
    Debug,
    Info,
    Warn,
    Error,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct TraceContext {
    pub run_id: Option<String>,
    pub flow_id: Option<String>,
    pub parent_flow_id: Option<String>,
    pub component: Option<String>,
    pub fields: BTreeMap<String, String>,
}

impl TraceContext {
    pub fn with_run_id(mut self, run_id: impl Into<String>) -> Self {
        self.run_id = Some(run_id.into());
        self
    }

    pub fn with_flow_id(mut self, flow_id: impl Into<String>) -> Self {
        self.flow_id = Some(flow_id.into());
        self
    }

    pub fn with_parent_flow_id(mut self, parent_flow_id: impl Into<String>) -> Self {
        self.parent_flow_id = Some(parent_flow_id.into());
        self
    }

    pub fn with_component(mut self, component: impl Into<String>) -> Self {
        self.component = Some(component.into());
        self
    }

    pub fn with_field(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.fields.insert(key.into(), value.into());
        self
    }
}

#[derive(Debug)]
pub struct TraceScopeGuard {
    previous: TraceContext,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TraceEvent {
    pub id: String,
    pub created_at_ms: u128,
    pub level: TraceLevel,
    pub component: String,
    pub stage: String,
    pub action: String,
    pub status: Option<String>,
    pub message: String,
    pub run_id: Option<String>,
    pub flow_id: Option<String>,
    pub parent_flow_id: Option<String>,
    pub fields: BTreeMap<String, String>,
}

impl TraceEvent {
    pub fn new(
        level: TraceLevel,
        component: impl Into<String>,
        stage: impl Into<String>,
        action: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            id: new_trace_id("evt"),
            created_at_ms: hc_bootstrap::wall_clock_ms() as u128,
            level,
            component: component.into(),
            stage: stage.into(),
            action: action.into(),
            status: None,
            message: message.into(),
            run_id: None,
            flow_id: None,
            parent_flow_id: None,
            fields: BTreeMap::new(),
        }
    }

    pub fn debug(
        component: impl Into<String>,
        stage: impl Into<String>,
        action: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self::new(TraceLevel::Debug, component, stage, action, message)
    }

    pub fn info(
        component: impl Into<String>,
        stage: impl Into<String>,
        action: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self::new(TraceLevel::Info, component, stage, action, message)
    }

    pub fn warn(
        component: impl Into<String>,
        stage: impl Into<String>,
        action: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self::new(TraceLevel::Warn, component, stage, action, message)
    }

    pub fn error(
        component: impl Into<String>,
        stage: impl Into<String>,
        action: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self::new(TraceLevel::Error, component, stage, action, message)
    }

    pub fn with_status(mut self, status: impl Into<String>) -> Self {
        self.status = Some(status.into());
        self
    }

    pub fn with_run_id(mut self, run_id: impl Into<String>) -> Self {
        self.run_id = Some(run_id.into());
        self
    }

    pub fn with_flow_id(mut self, flow_id: impl Into<String>) -> Self {
        self.flow_id = Some(flow_id.into());
        self
    }

    pub fn with_parent_flow_id(mut self, parent_flow_id: impl Into<String>) -> Self {
        self.parent_flow_id = Some(parent_flow_id.into());
        self
    }

    pub fn with_field(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.fields.insert(key.into(), value.into());
        self
    }
}

#[derive(Debug)]
pub struct TraceWriter {
    path: PathBuf,
    lock: Mutex<()>,
    mirror_to_stderr: bool,
}

impl TraceWriter {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            lock: Mutex::new(()),
            mirror_to_stderr: false,
        }
    }

    pub fn for_workspace_run(
        root: impl AsRef<Path>,
        namespace: &WorkspaceNamespace,
        app_name: &str,
        run_id: &str,
    ) -> Self {
        let path = root
            .as_ref()
            .join(namespace.scoped_prefix())
            .join("logs")
            .join("trace")
            .join(app_name)
            .join(format!("{run_id}.jsonl"));
        Self::new(path)
    }

    pub fn with_stderr_mirror(mut self, mirror_to_stderr: bool) -> Self {
        self.mirror_to_stderr = mirror_to_stderr;
        self
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn append(&self, event: &TraceEvent) -> std::io::Result<()> {
        let _guard = self
            .lock
            .lock()
            .expect("trace writer lock should not poison");
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        serde_json::to_writer(&mut file, event)?;
        file.write_all(b"\n")?;
        if self.mirror_to_stderr {
            eprintln!(
                "[trace:{}:{}:{}] {}",
                event.component, event.stage, event.action, event.message
            );
        }
        Ok(())
    }
}

thread_local! {
    static TRACE_CONTEXT: RefCell<TraceContext> = RefCell::new(TraceContext::default());
}

static TRACE_WRITER: OnceLock<TraceWriter> = OnceLock::new();
static TRACE_SEQUENCE: AtomicU64 = AtomicU64::new(1);

pub fn install_global_trace_writer(writer: TraceWriter) -> bool {
    TRACE_WRITER.set(writer).is_ok()
}

pub fn global_trace_path() -> Option<&'static Path> {
    TRACE_WRITER.get().map(TraceWriter::path)
}

pub fn current_trace_context() -> TraceContext {
    TRACE_CONTEXT.with(|context| context.borrow().clone())
}

pub fn replace_trace_context(context: TraceContext) -> TraceContext {
    TRACE_CONTEXT.with(|current| current.replace(context))
}

pub fn push_trace_context(context: TraceContext) -> TraceScopeGuard {
    let previous = TRACE_CONTEXT.with(|current| current.replace(context));
    TraceScopeGuard { previous }
}

impl Drop for TraceScopeGuard {
    fn drop(&mut self) {
        TRACE_CONTEXT.with(|current| {
            current.replace(self.previous.clone());
        });
    }
}

pub fn emit_trace(event: TraceEvent) {
    if let Some(writer) = TRACE_WRITER.get() {
        let _ = writer.append(&merge_trace_context(event));
    }
}

pub fn emit_info(
    component: impl Into<String>,
    stage: impl Into<String>,
    action: impl Into<String>,
    message: impl Into<String>,
) {
    emit_trace(TraceEvent::info(component, stage, action, message));
}

pub fn emit_warn(
    component: impl Into<String>,
    stage: impl Into<String>,
    action: impl Into<String>,
    message: impl Into<String>,
) {
    emit_trace(TraceEvent::warn(component, stage, action, message));
}

pub fn emit_error(
    component: impl Into<String>,
    stage: impl Into<String>,
    action: impl Into<String>,
    message: impl Into<String>,
) {
    emit_trace(TraceEvent::error(component, stage, action, message));
}

fn merge_trace_context(mut event: TraceEvent) -> TraceEvent {
    let context = current_trace_context();
    if event.run_id.is_none() {
        event.run_id = context.run_id;
    }
    if event.flow_id.is_none() {
        event.flow_id = context.flow_id;
    }
    if event.parent_flow_id.is_none() {
        event.parent_flow_id = context.parent_flow_id;
    }
    if event.component.trim().is_empty() {
        if let Some(component) = context.component {
            event.component = component;
        }
    }
    for (key, value) in context.fields {
        event.fields.entry(key).or_insert(value);
    }
    event
}

pub fn new_trace_id(prefix: &str) -> String {
    let sequence = TRACE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    format!(
        "{prefix}.{}.{}",
        hc_bootstrap::wall_clock_ms() as u128,
        sequence
    )
}

pub fn agent_code_from(role: &str, instance_id: &str) -> String {
    format!(
        "AGT-{}-{}",
        role.to_ascii_uppercase(),
        compact_code(instance_id)
    )
}

pub fn behavior_mode_code_from(responder_kind: Option<ResponderKind>) -> String {
    match responder_kind {
        Some(ResponderKind::Llm) => "MODE-LLM-AUTO".to_owned(),
        Some(ResponderKind::Human) => "MODE-HUMAN-MANUAL".to_owned(),
        Some(ResponderKind::Rule) => "MODE-RULE-AUTO".to_owned(),
        Some(ResponderKind::Script) => "MODE-SCRIPT-AUTO".to_owned(),
        None => "MODE-UNBOUND".to_owned(),
    }
}

pub fn code_from(prefix: &str, value: &str) -> String {
    format!("{prefix}-{}", compact_code(value))
}

pub fn summarize_trace_body(body: &str) -> String {
    const LIMIT: usize = 80;
    if body.chars().count() <= LIMIT {
        return body.to_owned();
    }

    let shortened = body.chars().take(LIMIT).collect::<String>();
    format!("{shortened}...")
}

pub fn compact_code(value: &str) -> String {
    value
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .take(8)
        .collect::<String>()
        .to_ascii_uppercase()
}

#[cfg(test)]
#[path = "../tests/unit/lib.rs"]
mod tests;
