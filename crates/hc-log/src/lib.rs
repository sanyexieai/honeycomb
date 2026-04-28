use hc_store::store::WorkspaceNamespace;
use hc_trace::{
    TraceContext, TraceEvent, TraceScopeGuard, TraceWriter, current_trace_context, emit_trace,
    global_trace_path, install_global_trace_writer, new_trace_id, push_trace_context,
    replace_trace_context,
};
use std::{
    collections::BTreeMap,
    io::{self, Write},
    path::Path,
    sync::{Mutex, OnceLock},
};

#[derive(Debug, Clone)]
pub struct CliLogger {
    component: String,
}

#[derive(Default)]
struct PromptDisplayState {
    active_prompt: bool,
    prompt_text: String,
}

static PROMPT_DISPLAY_STATE: OnceLock<Mutex<PromptDisplayState>> = OnceLock::new();
static TRACE_RUN_ID: OnceLock<String> = OnceLock::new();

impl CliLogger {
    pub fn init_for_local_workspace_run(
        workspace_root: impl AsRef<Path>,
        app_name: &str,
        component: impl Into<String>,
    ) -> Self {
        let namespace = WorkspaceNamespace::local_default();
        Self::init_for_workspace_run(workspace_root, &namespace, app_name, component)
    }

    pub fn init_for_workspace_run(
        workspace_root: impl AsRef<Path>,
        namespace: &WorkspaceNamespace,
        app_name: &str,
        component: impl Into<String>,
    ) -> Self {
        let component = component.into();
        let run_id = new_trace_id(&format!("run.{}", component.replace('-', "_")));
        let writer = TraceWriter::for_workspace_run(workspace_root, namespace, app_name, &run_id)
            .with_stderr_mirror(false);
        let _ = install_global_trace_writer(writer);
        let _ = TRACE_RUN_ID.set(run_id.clone());

        let base_context = TraceContext::default()
            .with_run_id(run_id.clone())
            .with_component(component.clone())
            .with_field("tenant_id", namespace.tenant_id.clone())
            .with_field("user_id", namespace.user_id.clone());
        replace_trace_context(base_context);

        let mut event =
            TraceEvent::info(&component, "runtime", "init", "initialized trace logging")
                .with_status("ready")
                .with_field("run_id", run_id);
        if let Some(path) = global_trace_path() {
            event = event.with_field("trace_path", path.display().to_string());
        }
        emit_trace(event);

        Self { component }
    }

    pub fn component(&self) -> &str {
        &self.component
    }

    pub fn current_run_id(&self) -> Option<String> {
        TRACE_RUN_ID.get().cloned()
    }

    pub fn enter_flow_context(&self, flow_id: impl Into<String>) -> TraceScopeGuard {
        let mut context = current_trace_context();
        context.flow_id = Some(flow_id.into());
        push_trace_context(context)
    }

    pub fn emit(
        &self,
        stage: &str,
        action: &str,
        status: Option<&str>,
        message: impl Into<String>,
    ) {
        let mut event = self.trace_event(stage, action, status, message);
        if let Some(status) = status {
            event = event.with_status(status);
        }
        emit_trace(event);
    }

    pub fn emit_with_fields(
        &self,
        stage: &str,
        action: &str,
        status: Option<&str>,
        message: impl Into<String>,
        fields: BTreeMap<String, String>,
    ) {
        let mut event = self.trace_event(stage, action, status, message);
        if let Some(status) = status {
            event = event.with_status(status);
        }
        for (key, value) in fields {
            event = event.with_field(key, value);
        }
        emit_trace(event);
    }

    pub fn print_status(&self, stage: &str, detail: impl AsRef<str>) {
        self.write_status(false, stage, detail.as_ref());
    }

    pub fn eprint_status(&self, stage: &str, detail: impl AsRef<str>) {
        self.write_status(true, stage, detail.as_ref());
    }

    pub fn set_active_prompt(&self, prompt: &str) {
        let mut state = prompt_display_state()
            .lock()
            .expect("prompt display state should lock");
        state.active_prompt = true;
        state.prompt_text = prompt.to_owned();
    }

    pub fn clear_active_prompt(&self) {
        let mut state = prompt_display_state()
            .lock()
            .expect("prompt display state should lock");
        state.active_prompt = false;
        state.prompt_text.clear();
    }

    fn trace_event(
        &self,
        stage: &str,
        action: &str,
        status: Option<&str>,
        message: impl Into<String>,
    ) -> TraceEvent {
        match status {
            Some("failed") => TraceEvent::error(&self.component, stage, action, message),
            Some("skipped") | Some("disabled") => {
                TraceEvent::warn(&self.component, stage, action, message)
            }
            _ => TraceEvent::info(&self.component, stage, action, message),
        }
    }

    fn write_status(&self, use_stderr: bool, stage: &str, detail: &str) {
        let fields = parse_status_fields(detail);
        let status = fields.get("status").cloned();
        self.emit_with_fields(
            stage,
            "status",
            status.as_deref(),
            detail.to_owned(),
            fields,
        );

        let (prompt_active, prompt_text) = {
            let state = prompt_display_state()
                .lock()
                .expect("prompt display state should lock");
            (state.active_prompt, state.prompt_text.clone())
        };
        let line = format!("organize> stage={stage} {detail}");

        if use_stderr {
            let mut stream = io::stderr().lock();
            if prompt_active {
                let _ = write!(stream, "\r\x1b[2K{line}\n{prompt_text}");
            } else {
                let _ = writeln!(stream, "{line}");
            }
            let _ = stream.flush();
        } else {
            let mut stream = io::stdout().lock();
            if prompt_active {
                let _ = write!(stream, "\r\x1b[2K{line}\n{prompt_text}");
            } else {
                let _ = writeln!(stream, "{line}");
            }
            let _ = stream.flush();
        }
    }
}

fn prompt_display_state() -> &'static Mutex<PromptDisplayState> {
    PROMPT_DISPLAY_STATE.get_or_init(|| Mutex::new(PromptDisplayState::default()))
}

fn parse_status_fields(detail: &str) -> BTreeMap<String, String> {
    let mut fields = BTreeMap::new();
    for token in detail.split_whitespace() {
        if let Some((key, value)) = token.split_once('=') {
            fields.insert(key.to_owned(), value.to_owned());
        }
    }
    fields
}
