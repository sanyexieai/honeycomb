use std::{
    cell::RefCell,
    collections::HashSet,
    io::{BufRead, BufReader, Read},
    process::{Command, Stdio},
    rc::Rc,
    sync::mpsc::{self, Receiver, Sender},
    thread,
    time::Duration,
};

use anyhow::{Context, Result, bail};
use hc_agent::{
    ActivityItemView, AgentOrchestrator, AgentPlan, AgentSeed, AgentWorkbench, MaterializedAgent,
    TaskArtifactSummary, TaskBudget, TaskNamespace, TaskPlan, TaskRequest, WorkspacePhase,
    append_implicit_intent_dedupe_record, append_routing_binding_log_line,
    append_work_item_assignment_journal_line, append_work_item_claim_journal_line,
    bootstrap_task_workbench, build_routing_binding_log_line_v1, build_workspace_view,
    hydrate_task_plan_work_item_coordination_journals, load_implicit_intent_dedupe_keys,
    materialize_seed, persist_task_artifacts_with_in_memory_prune, query_task_artifacts,
};
use hc_bootstrap::{tenant_id_from_env, user_id_from_env};
use hc_context::{
    load_agent_planner_input_prompt, load_agent_responder_system_prompt,
    load_agent_work_item_execution_prompt,
};
use hc_core::{
    MessageKind, MessageRoute, RuntimeCommand, RuntimeCommandResult, RuntimeNamespace,
    RuntimeSupervisor, SessionRecord,
};
use hc_llm::{
    ProviderRegistry, default_model_from_env, default_provider_from_env, default_registry_from_env,
    provider_api_key_from_env,
};
use hc_protocol::swarm::{
    ImplicitIntentDedupeKey, ImplicitIntentDedupeRecord, RoutingTier, TaskBindingAction,
    WorkItemLifecycleState,
};
use hc_responder::{
    HumanInboxRepository, HumanResponderConfig, LlmResponderConfig, ReplyRequest, ReplyResponse,
    ResponderBackend, ResponderBinding, require_human,
};
use serde::Deserialize;
use slint::{ComponentHandle, ModelRc, SharedString, Timer, TimerMode, VecModel, Weak};

#[derive(Debug, Clone, Deserialize)]
struct PlannerDraft {
    notes: Vec<String>,
    work_items: Vec<PlannerWorkItem>,
    agent_proposals: Vec<PlannerAgentProposal>,
}

#[derive(Debug, Clone, Deserialize)]
struct PlannerWorkItem {
    stage: String,
    title: String,
    goal: String,
}

#[derive(Debug, Clone, Deserialize)]
struct PlannerAgentProposal {
    role: String,
    reason: String,
}

slint::slint! {
    import { Button, HorizontalBox, LineEdit, TextEdit, VerticalBox } from "std-widgets.slint";

    export component StartTaskShell inherits Window {
        in property <string> window-title;
        callback start_task(string);

        title: window-title;
        width: 720px;
        height: 420px;

        Rectangle {
            background: #f7f1e6;

            VerticalBox {
                padding: 24px;
                spacing: 16px;

                Text {
                    text: "Honeycomb";
                    font-size: 28px;
                    font-weight: 700;
                }

                Text {
                    text: "Start with a task. Honeycomb will create a task workspace and spawn task-scoped agents for collaboration.";
                    wrap: word-wrap;
                    color: #5e584f;
                }

                Rectangle {
                    border-radius: 14px;
                    background: #fffdf8;
                    border-width: 1px;
                    border-color: #d7d1c6;

                    VerticalBox {
                        padding: 14px;
                        spacing: 10px;

                        Text {
                            text: "New Task";
                            font-size: 13px;
                            color: #666;
                        }

                        task_input := TextEdit {
                            text: "";
                            wrap: word-wrap;
                            height: 180px;
                        }

                        HorizontalBox {
                            spacing: 8px;

                            Button {
                                text: "Start Task";
                                clicked => {
                                    if !task_input.text.is-empty {
                                        root.start_task(task_input.text);
                                    }
                                }
                            }
                        }
                    }
                }

                Text {
                    text: "Examples: \"帮我拆一下这个任务\"  \"分析这个仓库并给出实现计划\"";
                    color: #777;
                }
            }
        }
    }

    export component MultiWindowShell inherits Window {
        in property <string> window-title;
        in property <string> role-name;
        in property <int> window-index;
        in property <[string]> open-window-titles;
        in property <string> agent-board-text;
        in property <string> agent-slot-1;
        in property <string> agent-slot-2;
        in property <string> agent-slot-3;
        in property <string> agent-slot-4;
        in property <string> agent-slot-5;
        in property <string> agent-slot-6;
        in property <string> work-items-text;
        in property <string> inspector-text;
        in property <string> transcript-text;
        in property <string> prompt-label;
        in property <string> selected-work-item-label;
        in property <string> work-item-slot-1;
        in property <string> work-item-slot-2;
        in property <string> work-item-slot-3;
        in property <string> work-item-slot-4;
        in property <string> work-item-slot-5;
        in property <string> work-item-slot-6;

        callback new_window();
        callback close_window();
        callback submit_input(string);
        callback focus_agent(string);
        callback clear_focus_target();
        callback select_work_item(string);
        callback claim_selected_work_item(string, string);
        callback resolve_selected_work_item();
        callback execute_selected_work_item();

        title: window-title;
        width: 980px;
        height: 620px;

        VerticalBox {
            spacing: 12px;
            padding: 16px;

            Text {
                text: "Honeycomb UI";
                font-size: 24px;
                font-weight: 700;
            }

            Text {
                text: prompt-label;
                color: #555;
            }

            HorizontalBox {
                spacing: 8px;

                Button {
                    text: "New Window";
                    clicked => {
                        root.new_window();
                    }
                }

                Button {
                    text: "Close";
                    enabled: window-index > 0;
                    clicked => {
                        root.close_window();
                    }
                }
            }

            HorizontalBox {
                spacing: 10px;
                vertical-stretch: 1;

                Rectangle {
                    min-width: 180px;
                    preferred-width: 210px;
                    border-radius: 12px;
                    background: #f4efe6;
                    border-width: 1px;
                    border-color: #d7d1c6;

                    VerticalBox {
                        padding: 10px;
                        spacing: 6px;

                        Text {
                            text: "Agents";
                            font-size: 13px;
                            color: #666;
                        }

                        HorizontalBox {
                            spacing: 6px;

                            Button {
                                text: agent-slot-1;
                                enabled: !agent-slot-1.is-empty;
                                clicked => {
                                    if !agent-slot-1.is-empty {
                                        root.focus_agent(agent-slot-1);
                                    }
                                }
                            }

                            Button {
                                text: agent-slot-2;
                                enabled: !agent-slot-2.is-empty;
                                clicked => {
                                    if !agent-slot-2.is-empty {
                                        root.focus_agent(agent-slot-2);
                                    }
                                }
                            }

                            Button {
                                text: agent-slot-3;
                                enabled: !agent-slot-3.is-empty;
                                clicked => {
                                    if !agent-slot-3.is-empty {
                                        root.focus_agent(agent-slot-3);
                                    }
                                }
                            }
                        }

                        HorizontalBox {
                            spacing: 6px;

                            Button {
                                text: agent-slot-4;
                                enabled: !agent-slot-4.is-empty;
                                clicked => {
                                    if !agent-slot-4.is-empty {
                                        root.focus_agent(agent-slot-4);
                                    }
                                }
                            }

                            Button {
                                text: agent-slot-5;
                                enabled: !agent-slot-5.is-empty;
                                clicked => {
                                    if !agent-slot-5.is-empty {
                                        root.focus_agent(agent-slot-5);
                                    }
                                }
                            }

                            Button {
                                text: agent-slot-6;
                                enabled: !agent-slot-6.is-empty;
                                clicked => {
                                    if !agent-slot-6.is-empty {
                                        root.focus_agent(agent-slot-6);
                                    }
                                }
                            }
                        }

                        Button {
                            text: "Workspace";
                            clicked => {
                                root.clear_focus_target();
                            }
                        }

                        TextEdit {
                            read-only: true;
                            text: agent-board-text;
                            vertical-stretch: 1;
                        }
                    }
                }

                Rectangle {
                    border-radius: 12px;
                    background: #faf7f0;
                    border-width: 1px;
                    border-color: #d7d1c6;
                    horizontal-stretch: 1;

                    VerticalBox {
                        padding: 10px;
                        spacing: 6px;

                        Text {
                            text: "Main Thread";
                            font-size: 13px;
                            color: #666;
                        }

                        TextEdit {
                            read-only: true;
                            text: transcript-text;
                            vertical-stretch: 1;
                        }
                    }
                }

                Rectangle {
                    min-width: 180px;
                    preferred-width: 220px;
                    border-radius: 12px;
                    background: #f8f8f8;
                    border-width: 1px;
                    border-color: #d7d1c6;

                    VerticalBox {
                        padding: 10px;
                        spacing: 6px;

                        Rectangle {
                            border-radius: 10px;
                            background: #fdfdfd;
                            border-width: 1px;
                            border-color: #d7d1c6;
                            preferred-height: 200px;

                            VerticalBox {
                                padding: 8px;
                                spacing: 6px;

                                Text {
                                    text: "Work Queue";
                                    font-size: 13px;
                                    color: #666;
                                }

                                TextEdit {
                                    read-only: true;
                                    text: work-items-text;
                                    vertical-stretch: 1;
                                }

                                Text {
                                    text: selected-work-item-label;
                                    color: #555;
                                }

                                HorizontalBox {
                                    spacing: 6px;

                                    Button {
                                        text: work-item-slot-1;
                                        enabled: !work-item-slot-1.is-empty;
                                        clicked => {
                                            if !work-item-slot-1.is-empty {
                                                root.select_work_item(work-item-slot-1);
                                            }
                                        }
                                    }

                                    Button {
                                        text: work-item-slot-2;
                                        enabled: !work-item-slot-2.is-empty;
                                        clicked => {
                                            if !work-item-slot-2.is-empty {
                                                root.select_work_item(work-item-slot-2);
                                            }
                                        }
                                    }

                                    Button {
                                        text: work-item-slot-3;
                                        enabled: !work-item-slot-3.is-empty;
                                        clicked => {
                                            if !work-item-slot-3.is-empty {
                                                root.select_work_item(work-item-slot-3);
                                            }
                                        }
                                    }
                                }

                                HorizontalBox {
                                    spacing: 6px;

                                    Button {
                                        text: work-item-slot-4;
                                        enabled: !work-item-slot-4.is-empty;
                                        clicked => {
                                            if !work-item-slot-4.is-empty {
                                                root.select_work_item(work-item-slot-4);
                                            }
                                        }
                                    }

                                    Button {
                                        text: work-item-slot-5;
                                        enabled: !work-item-slot-5.is-empty;
                                        clicked => {
                                            if !work-item-slot-5.is-empty {
                                                root.select_work_item(work-item-slot-5);
                                            }
                                        }
                                    }

                                    Button {
                                        text: work-item-slot-6;
                                        enabled: !work-item-slot-6.is-empty;
                                        clicked => {
                                            if !work-item-slot-6.is-empty {
                                                root.select_work_item(work-item-slot-6);
                                            }
                                        }
                                    }
                                }

                                HorizontalBox {
                                    spacing: 6px;

                                    selected_work_item_input := LineEdit {
                                        placeholder-text: "work-item.0001";
                                        accepted => {
                                            if !self.text.is-empty {
                                                root.select_work_item(self.text);
                                                self.text = "";
                                            }
                                        }
                                    }

                                    Button {
                                        text: "Use";
                                        clicked => {
                                            if !selected_work_item_input.text.is-empty {
                                                root.select_work_item(selected_work_item_input.text);
                                                selected_work_item_input.text = "";
                                            }
                                        }
                                    }
                                }

                                HorizontalBox {
                                    spacing: 6px;

                                    claim_score_input := LineEdit {
                                        placeholder-text: "score e.g. 0.85";
                                    }

                                    claim_reason_input := LineEdit {
                                        placeholder-text: "claim reason";
                                    }

                                    Button {
                                        text: "Claim";
                                        clicked => {
                                            if !claim_score_input.text.is-empty && !claim_reason_input.text.is-empty {
                                                root.claim_selected_work_item(claim_score_input.text, claim_reason_input.text);
                                                claim_reason_input.text = "";
                                            }
                                        }
                                    }
                                }

                                HorizontalBox {
                                    spacing: 6px;

                                    Button {
                                        text: "Resolve";
                                        clicked => {
                                            root.resolve_selected_work_item();
                                        }
                                    }

                                    Button {
                                        text: "Execute";
                                        clicked => {
                                            root.execute_selected_work_item();
                                        }
                                    }
                                }
                            }
                        }

                        Text {
                            text: "Inspector";
                            font-size: 13px;
                            color: #666;
                        }

                        TextEdit {
                            read-only: true;
                            text: inspector-text;
                            vertical-stretch: 1;
                        }
                    }
                }
            }

            HorizontalBox {
                spacing: 8px;

                input := LineEdit {
                    placeholder-text: "Describe what you want next. Planning uses natural language by default.";
                    accepted => {
                        if !self.text.is-empty {
                            root.submit_input(self.text);
                            self.text = "";
                        }
                    }
                }

                Button {
                    text: "Run";
                    clicked => {
                        if !input.text.is-empty {
                            root.submit_input(input.text);
                            input.text = "";
                        }
                    }
                }
            }

            Rectangle {
                border-radius: 12px;
                background: #ffffff;
                border-width: 1px;
                border-color: #d7d1c6;

                VerticalBox {
                    padding: 12px;
                    spacing: 6px;

                    Text {
                        text: "Open Windows";
                        font-size: 12px;
                        color: #666;
                    }

                    for title[index] in open-window-titles: Text {
                        text: (index + 1) + ". " + title;
                    }
                }
            }
        }
    }
}

struct WindowController {
    window_index: i32,
    instance_id: String,
    instance_name: String,
    role_name: String,
    focused_target: Option<String>,
    selected_work_item: Option<String>,
    pending_replies: Vec<ReplyRequest>,
    handle: MultiWindowShell,
    transcript_lines: Vec<String>,
}

struct UiRegistry {
    runtime: RuntimeSupervisor,
    orchestrator: AgentOrchestrator,
    session_id: String,
    /// Task id this runtime session treats as the active conversation binding (ADR-004); may diverge from workspace `task_id` later.
    conversation_active_task_id: Option<String>,
    task_id: String,
    task_title: String,
    task_goal: String,
    task_plan: TaskPlan,
    workspace_phase: WorkspacePhase,
    namespace: RuntimeNamespace,
    agents: Vec<MaterializedAgent>,
    task_artifacts: Vec<TaskArtifactSummary>,
    implicit_intent_seen: HashSet<ImplicitIntentDedupeKey>,
    windows: Vec<WindowController>,
    next_window_index: usize,
    events_rx: Receiver<UiEvent>,
    events_tx: Sender<UiEvent>,
}

struct RegistryReplyBackend {
    registry: ProviderRegistry,
}

impl ResponderBackend for RegistryReplyBackend {
    fn generate_reply(&self, request: &ReplyRequest) -> Result<ReplyResponse> {
        match &request.responder {
            ResponderBinding::Llm(llm) => {
                let mut messages = Vec::new();
                if let Some(system_prompt) = &llm.system_prompt {
                    messages.push(hc_llm::ChatMessage::new(
                        hc_llm::MessageRole::System,
                        system_prompt.clone(),
                    ));
                }
                messages.push(hc_llm::ChatMessage::new(
                    hc_llm::MessageRole::User,
                    request.source_body.clone(),
                ));

                let response = self
                    .registry
                    .generate(&hc_llm::GenerateRequest::new(
                        hc_llm::ModelRef::new(llm.provider.clone(), llm.model.clone()),
                        messages,
                    ))
                    .map_err(|error| anyhow::anyhow!(error))?;

                Ok(ReplyResponse::new(response.message.content))
            }
            ResponderBinding::Human(_) => {
                bail!(
                    "human responder cannot auto-generate replies; complete items in the human inbox"
                );
            }
            ResponderBinding::Rule(config) => {
                let profile = config.profile.as_deref().unwrap_or("default");
                Ok(ReplyResponse::new(format!(
                    "[rule:{profile}] {}",
                    request.source_body.trim()
                )))
            }
            ResponderBinding::Script(config) => {
                let body = run_script_responder_command(&config.command, request)?;
                if body.is_empty() {
                    Ok(ReplyResponse::new(
                        "[script] (empty stdout — set script to print reply text)",
                    ))
                } else {
                    Ok(ReplyResponse::new(body))
                }
            }
        }
    }
}

fn run_script_responder_command(command: &str, request: &ReplyRequest) -> Result<String> {
    if command.trim().is_empty() {
        bail!("script responder command cannot be empty");
    }
    let mut cmd = if cfg!(windows) {
        let mut c = Command::new("cmd.exe");
        c.arg("/C").arg(command);
        c
    } else {
        let mut c = Command::new("sh");
        c.arg("-c").arg(command);
        c
    };
    cmd.env("HC_SOURCE_BODY", &request.source_body);
    cmd.env("HC_SOURCE_MESSAGE_ID", &request.source_message_id);
    cmd.env("HC_SOURCE_SESSION_ID", &request.source_session_id);
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    let output = cmd
        .output()
        .with_context(|| format!("failed to run script responder: {command}"))?;
    if !output.status.success() {
        bail!(
            "script responder exited with {:?}: {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

enum UiEvent {
    CommandOutput { window_index: i32, line: String },
    CommandExit { window_index: i32, status: String },
}

pub fn run() -> Result<()> {
    let start_window = StartTaskShell::new()?;
    start_window.set_window_title("Honeycomb | New Task".into());

    let app_registry: Rc<RefCell<Option<Rc<RefCell<UiRegistry>>>>> = Rc::new(RefCell::new(None));
    let timer = Rc::new(Timer::default());

    {
        let start_window_handle = start_window.as_weak();
        let app_registry = Rc::clone(&app_registry);
        let timer = Rc::clone(&timer);
        start_window.on_start_task(move |goal_text| {
            let result: Result<()> = (|| {
                let task_goal = goal_text.trim().to_string();
                if task_goal.is_empty() {
                    bail!("task cannot be empty");
                }

                let (registry, primary_agent) = build_registry_for_task(&task_goal)?;

                timer.start(TimerMode::Repeated, Duration::from_millis(100), {
                    let registry = Rc::clone(&registry);
                    move || {
                        pump_ui_events(&registry);
                    }
                });

                spawn_materialized_window(Rc::clone(&registry), primary_agent)?;
                if let Err(error) = run_planner_natural_language_input(
                    &registry,
                    &format!("Initial task request: {}", task_goal.trim()),
                    true,
                ) {
                    append_local_line(
                        &registry,
                        0,
                        format!("[planning] automatic planner draft unavailable: {error}"),
                    );
                    sync_windows(&registry);
                }
                *app_registry.borrow_mut() = Some(registry);

                if let Some(window) = start_window_handle.upgrade() {
                    let _ = window.hide();
                }
                Ok(())
            })();

            if let Err(error) = result {
                tracing::warn!(%error, "failed to start task");
            }
        });
    }

    start_window.run()?;
    Ok(())
}

fn build_registry_for_task(
    task_goal: &str,
) -> Result<(Rc<RefCell<UiRegistry>>, MaterializedAgent)> {
    let mut runtime = RuntimeSupervisor::new();
    let namespace = runtime_namespace();
    let task_id = format!("task.ui.{}", current_timestamp_ms());
    let task_title = summarize_task_title(task_goal);
    let mut workbench = bootstrap_task_workbench(
        &mut runtime,
        TaskRequest::new(task_id, task_title, task_goal.to_owned()).with_namespace(
            TaskNamespace::new(namespace.tenant_id.clone(), namespace.user_id.clone()),
        ),
    )?;
    for agent in &mut workbench.agents {
        configure_default_responder(agent);
    }
    let (events_tx, events_rx) = mpsc::channel();
    let primary_agent = workbench
        .agents
        .first()
        .cloned()
        .context("expected at least one materialized agent window")?;

    let store_namespace = hc_store::store::WorkspaceNamespace::new(
        namespace.tenant_id.clone(),
        namespace.user_id.clone(),
    );
    let implicit_intent_seen = load_implicit_intent_dedupe_keys(
        hc_bootstrap::workspace_root(),
        &store_namespace,
        &workbench.task.id,
    )
    .unwrap_or_else(|error| {
        tracing::warn!(
            ?error,
            "load implicit-intent dedupe keys from workspace; continuing with empty set"
        );
        HashSet::new()
    });

    let registry = Rc::new(RefCell::new(UiRegistry {
        runtime,
        orchestrator: AgentOrchestrator::new(),
        session_id: workbench.session.id.clone(),
        conversation_active_task_id: Some(workbench.task.id.clone()),
        task_id: workbench.task.id.clone(),
        task_title: workbench.task.title.clone(),
        task_goal: workbench.task.goal.clone(),
        task_plan: workbench.task_plan.clone(),
        workspace_phase: workbench.phase.clone(),
        namespace,
        agents: workbench.agents,
        task_artifacts: Vec::new(),
        implicit_intent_seen,
        windows: Vec::new(),
        next_window_index: 0,
        events_rx,
        events_tx,
    }));

    {
        let mut registry_ref = registry.borrow_mut();
        let namespace = hc_store::store::WorkspaceNamespace::new(
            registry_ref.namespace.tenant_id.clone(),
            registry_ref.namespace.user_id.clone(),
        );
        let task_id = registry_ref.task_id.clone();
        hydrate_task_plan_work_item_coordination_journals(
            hc_bootstrap::workspace_root(),
            &namespace,
            &task_id,
            &mut registry_ref.task_plan,
        )?;
        refresh_persisted_task_artifacts(&mut registry_ref)?;
    }

    Ok((registry, primary_agent))
}

fn spawn_materialized_window(
    registry: Rc<RefCell<UiRegistry>>,
    agent: MaterializedAgent,
) -> Result<MultiWindowShell> {
    let window = MultiWindowShell::new()?;
    let weak = window.as_weak();

    let window_index = {
        let mut registry_ref = registry.borrow_mut();
        let next = registry_ref.next_window_index;
        registry_ref.next_window_index += 1;
        next
    };

    let namespace_label = {
        let registry_ref = registry.borrow();
        format!(
            "{}/{}",
            registry_ref.namespace.tenant_id, registry_ref.namespace.user_id
        )
    };
    let capability_list = if agent.capabilities.is_empty() {
        "none".to_owned()
    } else {
        agent
            .capabilities
            .iter()
            .map(|capability| capability.name.clone())
            .collect::<Vec<_>>()
            .join(", ")
    };

    window.set_window_title(
        format!("{} | Honeycomb | {}", agent.persona.name, namespace_label).into(),
    );
    window.set_role_name(agent.persona.name.clone().into());
    window.set_window_index(window_index as i32);
    window.set_prompt_label(
        format!(
            "{} | role {} | {}",
            agent.persona.name, agent.persona.role, namespace_label
        )
        .into(),
    );
    window.set_selected_work_item_label("Selected work item: none".into());

    {
        let registry = Rc::clone(&registry);
        window.on_new_window(move || {
            if let Err(error) = spawn_seeded_window(Rc::clone(&registry)) {
                tracing::warn!(%error, "failed to open window");
            }
        });
    }

    {
        let registry = Rc::clone(&registry);
        let window_index = window_index as i32;
        window.on_close_window(move || {
            if let Err(error) = close_window(Rc::clone(&registry), &weak, window_index) {
                tracing::warn!(%error, "failed to close window");
            }
        });
    }

    {
        let registry = Rc::clone(&registry);
        let window_index = window_index as i32;
        window.on_submit_input(move |text| {
            if let Err(error) =
                handle_window_input(Rc::clone(&registry), window_index, text.to_string())
            {
                tracing::warn!(%error, "input error");
            }
        });
    }

    {
        let registry = Rc::clone(&registry);
        let window_index = window_index as i32;
        window.on_focus_agent(move |agent_name| {
            if let Err(error) = set_window_focus_target(&registry, window_index, agent_name.trim())
            {
                tracing::warn!(%error, "focus agent error");
            }
        });
    }

    {
        let registry = Rc::clone(&registry);
        let window_index = window_index as i32;
        window.on_clear_focus_target(move || {
            if let Err(error) = clear_window_focus_target(&registry, window_index) {
                tracing::warn!(%error, "clear focus error");
            }
        });
    }

    {
        let registry = Rc::clone(&registry);
        let window_index = window_index as i32;
        window.on_select_work_item(move |work_item_id| {
            if let Err(error) =
                set_selected_work_item(&registry, window_index, work_item_id.to_string())
            {
                tracing::warn!(%error, "select work item error");
            }
        });
    }

    {
        let registry = Rc::clone(&registry);
        let window_index = window_index as i32;
        {
            let registry = Rc::clone(&registry);
            let window_index = window_index as i32;
            window.on_claim_selected_work_item(move |score_text, reason_text| {
                if let Err(error) = claim_selected_work_item(
                    &registry,
                    window_index,
                    score_text.to_string(),
                    reason_text.to_string(),
                ) {
                    tracing::warn!(%error, "claim work item error");
                }
            });
        }

        window.on_resolve_selected_work_item(move || {
            if let Err(error) = resolve_selected_work_item(&registry, window_index) {
                tracing::warn!(%error, "resolve work item error");
            }
        });
    }

    {
        let registry = Rc::clone(&registry);
        let window_index = window_index as i32;
        window.on_execute_selected_work_item(move || {
            if let Err(error) = execute_selected_work_item(&registry, window_index) {
                tracing::warn!(%error, "execute work item error");
            }
        });
    }

    window.show()?;

    {
        let mut registry_ref = registry.borrow_mut();
        let responder_label = agent
            .binding
            .responder
            .as_ref()
            .map(|responder| responder.label())
            .unwrap_or_else(|| "not bound".to_owned());
        let mut transcript_lines = vec![
            "[task] workspace created".to_owned(),
            format!("[task] namespace: {}", namespace_label),
            format!("[task] goal: {}", agent.seed.goal),
            "[planning] phase started".to_owned(),
            format!(
                "[planning] planner: {} ({})",
                agent.persona.name, agent.persona.role
            ),
            format!("[planning] capabilities: {capability_list}"),
            format!("[planning] responder: {responder_label}"),
        ];
        if agent
            .binding
            .responder
            .as_ref()
            .is_some_and(|responder| matches!(responder, ResponderBinding::Llm(_)))
        {
            transcript_lines.push(
                "[planning] Honeycomb will try to draft the initial plan automatically from the task description."
                    .to_owned(),
            );
            transcript_lines.push(
                "[planning] You can keep typing natural language to refine the plan.".to_owned(),
            );
        } else {
            transcript_lines.push(
                "[planning] Planner is currently human-driven. Type natural language planning guidance and Honeycomb will record it as planning notes."
                    .to_owned(),
            );
            transcript_lines.push(
                "[planning] The task will stay in planning until work items and agent proposals are added."
                    .to_owned(),
            );
        }

        registry_ref.windows.push(WindowController {
            window_index: window_index as i32,
            instance_id: agent.binding.instance_id.clone(),
            instance_name: agent.persona.name.clone(),
            role_name: agent.persona.role.clone(),
            focused_target: None,
            selected_work_item: None,
            pending_replies: Vec::new(),
            handle: window.clone_strong(),
            transcript_lines,
        });
    }

    sync_windows(&registry);
    Ok(window)
}

fn spawn_seeded_window(registry: Rc<RefCell<UiRegistry>>) -> Result<MultiWindowShell> {
    let agent = {
        let mut registry_ref = registry.borrow_mut();
        let seed_number = registry_ref.next_window_index + 1;
        let role = match seed_number % 3 {
            1 => "worker",
            2 => "reviewer",
            _ => "planner",
        };
        let seed = AgentSeed::new(
            format!("{}.{}", registry_ref.task_id, seed_number),
            format!("{role}-{seed_number}"),
            role,
            format!("Support task {} as {}", registry_ref.task_id, role),
        );
        let namespace = TaskNamespace::new(
            registry_ref.namespace.tenant_id.clone(),
            registry_ref.namespace.user_id.clone(),
        );
        let session_id = registry_ref.session_id.clone();
        let task_id = registry_ref.task_id.clone();
        let mut agent = materialize_seed(
            &mut registry_ref.runtime,
            &session_id,
            &task_id,
            &namespace,
            &seed,
        )?;
        configure_default_responder(&mut agent);
        registry_ref.agents.push(agent.clone());
        agent
    };

    spawn_materialized_window(registry, agent)
}

fn close_window(
    registry: Rc<RefCell<UiRegistry>>,
    weak: &Weak<MultiWindowShell>,
    window_index: i32,
) -> Result<()> {
    let Some(window) = weak.upgrade() else {
        return Ok(());
    };

    window.hide()?;

    {
        let mut registry_ref = registry.borrow_mut();
        registry_ref
            .windows
            .retain(|candidate| candidate.window_index != window_index);
    }

    sync_windows(&registry);
    Ok(())
}

fn handle_window_input(
    registry: Rc<RefCell<UiRegistry>>,
    window_index: i32,
    text: String,
) -> Result<()> {
    let line = text.trim();
    if line.is_empty() {
        return Ok(());
    }

    let workspace_phase = {
        let registry_ref = registry.borrow();
        registry_ref.workspace_phase.clone()
    };

    if let Some((target_name, body)) = parse_msg_command(line) {
        send_window_message(&registry, window_index, &target_name, &body)?;
        return Ok(());
    }

    if let Some((target_name, body)) = parse_targeted_message(line) {
        send_window_message(&registry, window_index, &target_name, &body)?;
        return Ok(());
    }

    if let Some(target_name) = parse_focus_command(line) {
        set_window_focus_target(&registry, window_index, &target_name)?;
        return Ok(());
    }

    if let Some(mode) = parse_responder_command(line) {
        set_window_responder(&registry, window_index, &mode)?;
        return Ok(());
    }

    if let Some(body) = parse_reply_command(line) {
        post_human_reply(&registry, window_index, &body)?;
        return Ok(());
    }

    if let Some(body) = parse_all_command(line) {
        broadcast_window_message(&registry, window_index, &body)?;
        return Ok(());
    }

    if let Some((channel_name, body)) = parse_channel_message_command(line) {
        send_channel_message(&registry, window_index, &channel_name, &body)?;
        return Ok(());
    }

    if let Some(channel_name) = parse_channel_create_command(line) {
        create_channel(&registry, window_index, &channel_name)?;
        return Ok(());
    }

    if let Some(channel_name) = parse_join_command(line) {
        join_channel(&registry, window_index, &channel_name)?;
        return Ok(());
    }

    if let Some(channel_name) = parse_leave_command(line) {
        leave_channel(&registry, window_index, &channel_name)?;
        return Ok(());
    }

    if let Some(name) = parse_name_command(line) {
        rename_window_instance(&registry, window_index, &name)?;
        return Ok(());
    }

    if let Some(command) = parse_run_command(line) {
        append_local_line(&registry, window_index, format!("$ {command}"));
        run_local_command_async(Rc::clone(&registry), window_index, command);
        sync_windows(&registry);
        return Ok(());
    }

    if let Some(note) = parse_plan_note_command(line) {
        add_plan_note(&registry, window_index, &note)?;
        return Ok(());
    }

    if let Some((stage, title, goal)) = parse_plan_stage_command(line) {
        add_plan_stage(&registry, window_index, &stage, &title, &goal)?;
        return Ok(());
    }

    if let Some((role, reason)) = parse_plan_agent_command(line) {
        add_plan_agent_proposal(&registry, window_index, &role, &reason)?;
        return Ok(());
    }

    if let Some((work_item_id, agent_name, score, reason)) = parse_assign_claim_command(line) {
        add_work_item_claim(
            &registry,
            window_index,
            &work_item_id,
            &agent_name,
            score,
            &reason,
        )?;
        return Ok(());
    }

    match line {
        "/help" => {
            append_local_line(
                &registry,
                window_index,
                "Commands: /msg <name> <text>, @name <text>, /focus <name>, /responder human|llm, /pending, /reply <text>, /all <text>, /channel <name> <text>, /run <command>, /plan note <text>, /plan stage <stage> | <title> | <goal>, /plan agent <role> | <reason>, /plan approve, /assign claim <work-item-id> | <agent> | <score> | <reason>, /assign resolve <work-item-id>, /execute <work-item-id>, /channel-create <name>, /join <name>, /leave <name>, /channels, /name <new-name>, /who, /help".to_owned(),
            );
        }
        "/plan approve" => {
            approve_plan(&registry, window_index)?;
        }
        _ if line.starts_with("/assign resolve ") => {
            let work_item_id = line.trim_start_matches("/assign resolve ").trim();
            resolve_work_item_assignment(&registry, window_index, work_item_id)?;
        }
        _ if line.starts_with("/execute ") => {
            let work_item_id = line.trim_start_matches("/execute ").trim();
            execute_work_item(&registry, window_index, work_item_id)?;
        }
        "/pending" => {
            let lines = list_pending_replies(&registry, window_index)?;
            for line in lines {
                append_local_line(&registry, window_index, line);
            }
        }
        "/who" => {
            let names = {
                let registry_ref = registry.borrow();
                registry_ref
                    .windows
                    .iter()
                    .map(|window| format!("{} ({})", window.instance_name, window.role_name))
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            append_local_line(&registry, window_index, format!("Open instances: {names}"));
        }
        "/channels" => {
            let lines = list_window_channels(&registry, window_index)?;
            for line in lines {
                append_local_line(&registry, window_index, line);
            }
        }
        _ => {
            if matches!(workspace_phase, WorkspacePhase::Planning) {
                run_planner_natural_language_input(&registry, line, false)?;
                return Ok(());
            }
            if let Some(target_name) = window_focus_target(&registry, window_index) {
                send_window_message(&registry, window_index, &target_name, line)?;
            } else {
                broadcast_window_message(&registry, window_index, line)?;
            }
            return Ok(());
        }
    }

    sync_windows(&registry);
    Ok(())
}

fn rename_window_instance(
    registry: &Rc<RefCell<UiRegistry>>,
    window_index: i32,
    new_name: &str,
) -> Result<()> {
    {
        let mut registry_ref = registry.borrow_mut();
        if registry_ref
            .windows
            .iter()
            .any(|window| window.instance_name == new_name)
        {
            bail!("instance name already exists: {new_name}");
        }

        let target_index = registry_ref
            .windows
            .iter()
            .position(|window| window.window_index == window_index)
            .ok_or_else(|| anyhow::anyhow!("window not found: {window_index}"))?;
        let instance_id = registry_ref.windows[target_index].instance_id.clone();

        let result = registry_ref
            .runtime
            .dispatch(RuntimeCommand::RenameInstance {
                instance_id,
                name: new_name.to_owned(),
            })?;
        let RuntimeCommandResult::Instance(instance) = result else {
            bail!("unexpected runtime result while renaming instance");
        };

        registry_ref.windows[target_index].instance_name = instance.name.clone();
        let namespace_label = format!(
            "{}/{}",
            registry_ref.namespace.tenant_id, registry_ref.namespace.user_id
        );
        registry_ref.windows[target_index].handle.set_window_title(
            format!("{} | Honeycomb | {}", instance.name, namespace_label).into(),
        );
        registry_ref.windows[target_index]
            .transcript_lines
            .push(format!("instance renamed to {}", instance.name));
        registry_ref.windows[target_index]
            .handle
            .set_role_name(instance.name.clone().into());
    }

    sync_windows(registry);
    Ok(())
}

fn set_window_focus_target(
    registry: &Rc<RefCell<UiRegistry>>,
    window_index: i32,
    target_name: &str,
) -> Result<()> {
    let mut registry_ref = registry.borrow_mut();
    if !registry_ref
        .windows
        .iter()
        .any(|window| window.instance_name == target_name)
    {
        bail!("target instance not found: {target_name}");
    }

    let target_index = registry_ref
        .windows
        .iter()
        .position(|window| window.window_index == window_index)
        .ok_or_else(|| anyhow::anyhow!("window not found: {window_index}"))?;

    registry_ref.windows[target_index].focused_target = Some(target_name.to_owned());
    registry_ref.windows[target_index]
        .transcript_lines
        .push(format!("focus target set to {target_name}"));
    drop(registry_ref);
    sync_windows(registry);
    Ok(())
}

fn window_focus_target(registry: &Rc<RefCell<UiRegistry>>, window_index: i32) -> Option<String> {
    let registry_ref = registry.borrow();
    registry_ref
        .windows
        .iter()
        .find(|window| window.window_index == window_index)
        .and_then(|window| window.focused_target.clone())
}

fn set_window_responder(
    registry: &Rc<RefCell<UiRegistry>>,
    window_index: i32,
    mode: &str,
) -> Result<()> {
    let mut registry_ref = registry.borrow_mut();
    let target_index = registry_ref
        .windows
        .iter()
        .position(|window| window.window_index == window_index)
        .ok_or_else(|| anyhow::anyhow!("window not found: {window_index}"))?;
    let instance_id = registry_ref.windows[target_index].instance_id.clone();
    let instance_name = registry_ref.windows[target_index].instance_name.clone();
    let role_name = registry_ref.windows[target_index].role_name.clone();

    let binding = match mode {
        "human" => ResponderBinding::Human(HumanResponderConfig::new(
            Some(registry_ref.namespace.user_id.clone()),
            Some(instance_id.clone()),
        )),
        "llm" => ResponderBinding::Llm(LlmResponderConfig {
            provider: default_provider_from_env(),
            model: default_model_from_env(),
            system_prompt: Some(render_agent_responder_system_prompt(
                &registry_ref.namespace,
                &instance_name,
                &role_name,
                "Stay concise and collaborative.",
            )?),
        }),
        _ => bail!("unknown responder mode: {mode}"),
    };

    if let Some(agent) = registry_ref
        .agents
        .iter_mut()
        .find(|agent| agent.binding.instance_id == instance_id)
    {
        agent.binding.responder_binding_ref = Some(format!("responder.{mode}"));
        agent.binding.responder = Some(binding.clone());
    } else {
        bail!("agent binding not found for instance: {instance_id}");
    }

    registry_ref.windows[target_index]
        .transcript_lines
        .push(format!("responder mode set to {}", binding.label()));
    drop(registry_ref);
    sync_windows(registry);
    Ok(())
}

fn queue_human_reply(registry_ref: &mut UiRegistry, request: ReplyRequest) -> Result<()> {
    let human = require_human(&request.responder)?;
    let responder_user_ref = human
        .user_ref
        .clone()
        .unwrap_or_else(|| registry_ref.namespace.user_id.clone());
    let queue_id = human
        .queue_id
        .clone()
        .unwrap_or_else(|| "default".to_owned());
    let repository = HumanInboxRepository::with_namespace(
        hc_bootstrap::workspace_root(),
        hc_store::store::WorkspaceNamespace::new(
            registry_ref.namespace.tenant_id.clone(),
            responder_user_ref.clone(),
        ),
    );
    let item = hc_responder::HumanInboxItem::from_reply_request(
        &request,
        responder_user_ref.clone(),
        queue_id,
        current_timestamp_ms(),
    );
    repository.write_pending(&item)?;

    let target_index = registry_ref
        .windows
        .iter()
        .position(|window| window.instance_id == request.replying_instance_id)
        .ok_or_else(|| anyhow::anyhow!("window not found for human responder"))?;
    let sender_name = registry_ref
        .windows
        .iter()
        .find(|window| window.instance_id == request.source_from_instance_id)
        .map(|window| window.instance_name.clone())
        .unwrap_or_else(|| request.source_from_instance_id.clone());

    registry_ref.windows[target_index]
        .transcript_lines
        .push(format!(
            "[pending human reply] routed to {} from {}: {}",
            responder_user_ref, sender_name, request.source_body
        ));
    registry_ref.windows[target_index]
        .transcript_lines
        .push("[hint] reply locally with /reply ... or remotely with `hc-responder-cli inbox reply-next <text>`".to_owned());
    if responder_user_ref == registry_ref.namespace.user_id {
        registry_ref.windows[target_index]
            .pending_replies
            .push(request);
    }
    Ok(())
}

fn post_human_reply(
    registry: &Rc<RefCell<UiRegistry>>,
    window_index: i32,
    body: &str,
) -> Result<()> {
    {
        let mut registry_ref = registry.borrow_mut();
        let source_index = registry_ref
            .windows
            .iter()
            .position(|window| window.window_index == window_index)
            .ok_or_else(|| anyhow::anyhow!("window not found: {window_index}"))?;
        if registry_ref.windows[source_index]
            .pending_replies
            .is_empty()
        {
            bail!("no pending human replies");
        }

        let request = registry_ref.windows[source_index].pending_replies.remove(0);
        let source_name = registry_ref.windows[source_index].instance_name.clone();
        let repository = HumanInboxRepository::with_namespace(
            hc_bootstrap::workspace_root(),
            hc_store::store::WorkspaceNamespace::new(
                registry_ref.namespace.tenant_id.clone(),
                registry_ref.namespace.user_id.clone(),
            ),
        );
        let result = registry_ref.runtime.dispatch(RuntimeCommand::PostMessage {
            session_id: request.source_session_id.clone(),
            from: request.replying_instance_id.clone(),
            route: MessageRoute::Direct {
                to: request.source_from_instance_id.clone(),
            },
            kind: MessageKind::Chat,
            body: body.to_owned(),
            reply_to: Some(request.source_message_id.clone()),
        })?;
        let RuntimeCommandResult::Message(reply) = result else {
            bail!("unexpected runtime result while posting human reply");
        };

        registry_ref.windows[source_index]
            .transcript_lines
            .push(format!(
                "[manual reply {}] you -> {}: {}",
                reply.id, request.source_from_instance_id, reply.body
            ));

        if let Some(target_window) = registry_ref
            .windows
            .iter_mut()
            .find(|window| window.instance_id == request.source_from_instance_id)
        {
            target_window.transcript_lines.push(format!(
                "[recv {}] {} -> you: {}",
                reply.id, source_name, reply.body
            ));
        }
        let item_id = format!(
            "human-inbox.{}.{}",
            request.source_message_id, request.replying_instance_id
        );
        let _ = repository.complete_pending(&item_id, body, current_timestamp_ms());
    }
    sync_windows(registry);
    Ok(())
}

fn send_window_message(
    registry: &Rc<RefCell<UiRegistry>>,
    from_window_index: i32,
    target_name: &str,
    body: &str,
) -> Result<()> {
    {
        let mut registry_ref = registry.borrow_mut();

        let from_index = registry_ref
            .windows
            .iter()
            .position(|window| window.window_index == from_window_index)
            .ok_or_else(|| anyhow::anyhow!("window not found: {from_window_index}"))?;
        let to_index = registry_ref
            .windows
            .iter()
            .position(|window| window.instance_name == target_name)
            .ok_or_else(|| anyhow::anyhow!("target instance not found: {target_name}"))?;

        let session_id = registry_ref.session_id.clone();
        let from_id = registry_ref.windows[from_index].instance_id.clone();
        let from_name = registry_ref.windows[from_index].instance_name.clone();
        let to_id = registry_ref.windows[to_index].instance_id.clone();
        let to_name = registry_ref.windows[to_index].instance_name.clone();

        let result = registry_ref.runtime.dispatch(RuntimeCommand::PostMessage {
            session_id: session_id.clone(),
            from: from_id.clone(),
            route: MessageRoute::Direct { to: to_id.clone() },
            kind: MessageKind::Chat,
            body: body.to_owned(),
            reply_to: None,
        })?;
        let RuntimeCommandResult::Message(message) = result else {
            bail!("unexpected runtime result while posting message");
        };

        registry_ref.windows[from_index]
            .transcript_lines
            .push(format!("[you -> {to_name}] {}", message.body));
        registry_ref.windows[to_index]
            .transcript_lines
            .push(format!(
                "[recv {}] {from_name} -> you: {}",
                message.id, message.body
            ));

        let orchestrator = registry_ref.orchestrator.clone();
        let agents = registry_ref.agents.clone();
        if let Some(agent) = agents
            .iter()
            .find(|agent| agent.binding.instance_id == to_id)
        {
            if let Some(responder) = agent.binding.responder.as_ref() {
                if responder.is_human() {
                    let request = orchestrator.build_direct_reply_request(
                        &registry_ref.runtime,
                        &agents,
                        &message.id,
                        &to_id,
                    )?;
                    queue_human_reply(&mut registry_ref, request)?;
                    registry_ref.windows[from_index]
                        .transcript_lines
                        .push(format!("[pending] {} will reply manually", to_name));
                } else {
                    let reply_backend = default_reply_backend();
                    match orchestrator.generate_and_post_direct_reply(
                        &reply_backend,
                        &mut registry_ref.runtime,
                        &agents,
                        &message.id,
                        &to_id,
                    ) {
                        Ok(reply) => {
                            registry_ref.windows[to_index]
                                .transcript_lines
                                .push(format!(
                                    "[reply {}] you -> {}: {}",
                                    reply.id, from_name, reply.body
                                ));
                            registry_ref.windows[from_index]
                                .transcript_lines
                                .push(format!(
                                    "[recv {}] {} -> you: {}",
                                    reply.id, to_name, reply.body
                                ));
                        }
                        Err(error) => {
                            registry_ref.windows[from_index]
                                .transcript_lines
                                .push(format!("[llm error] {} could not reply: {error}", to_name));
                        }
                    }
                }
            } else {
                registry_ref.windows[from_index]
                    .transcript_lines
                    .push(format!("[responder] {} has no responder bound", to_name));
            }
        }
    }
    sync_windows(registry);
    Ok(())
}

fn broadcast_window_message(
    registry: &Rc<RefCell<UiRegistry>>,
    from_window_index: i32,
    body: &str,
) -> Result<()> {
    let mut steer_planner_nl_followup = None::<String>;
    let mut assignment_routed_followup = false;
    {
        let mut registry_ref = registry.borrow_mut();
        let from_index = registry_ref
            .windows
            .iter()
            .position(|window| window.window_index == from_window_index)
            .ok_or_else(|| anyhow::anyhow!("window not found: {from_window_index}"))?;

        let session_id = registry_ref.session_id.clone();
        let from_id = registry_ref.windows[from_index].instance_id.clone();
        let from_name = registry_ref.windows[from_index].instance_name.clone();

        let result = registry_ref.runtime.dispatch(RuntimeCommand::PostMessage {
            session_id,
            from: from_id,
            route: MessageRoute::Broadcast,
            kind: MessageKind::Chat,
            body: body.to_owned(),
            reply_to: None,
        })?;
        let RuntimeCommandResult::Message(message) = result else {
            bail!("unexpected runtime result while posting broadcast");
        };

        for window in &mut registry_ref.windows {
            if window.window_index == from_window_index {
                window
                    .transcript_lines
                    .push(format!("[you -> *] {}", message.body));
            } else {
                window.transcript_lines.push(format!(
                    "[broadcast {}] {from_name}: {}",
                    message.id, message.body
                ));
            }
        }

        let orchestrator = registry_ref.orchestrator.clone();
        let agents = registry_ref.agents.clone();
        let conversation_active = registry_ref.conversation_active_task_id.clone();
        let task_scope_id = registry_ref.task_id.clone();
        let ts = current_timestamp_ms();
        let nomination = orchestrator.run_nomination_cycle(
            &mut registry_ref.runtime,
            &agents,
            &message,
            ts,
            conversation_active.as_deref(),
            Some(task_scope_id.as_str()),
        )?;
        if let Some(ref swarm) = nomination.swarm {
            registry_ref.conversation_active_task_id = swarm.task_binding.active_task_id.clone();

            if swarm.task_binding.task_binding_action == TaskBindingAction::CreateImplicitTask {
                let key = ImplicitIntentDedupeKey::from_trigger(
                    message.session_id.clone(),
                    message.id.clone(),
                    &message.body,
                );
                if registry_ref.implicit_intent_seen.contains(&key) {
                    tracing::info!(
                        message_id = %message.id,
                        session_id = %message.session_id,
                        "implicit work intent dedupe: duplicate ADR-003 key, skip journal append"
                    );
                } else {
                    let record = ImplicitIntentDedupeRecord::from_key(&key, ts);
                    if let Err(error) = append_implicit_intent_dedupe_record(
                        hc_bootstrap::workspace_root(),
                        &workspace_namespace(&registry_ref.namespace),
                        task_scope_id.as_str(),
                        &record,
                    ) {
                        tracing::warn!(?error, "append implicit intent dedupe record");
                    } else {
                        registry_ref.implicit_intent_seen.insert(key);
                    }
                }
            }

            let line =
                build_routing_binding_log_line_v1(ts, &message, task_scope_id.as_str(), swarm);
            if let Err(error) = append_routing_binding_log_line(
                hc_bootstrap::workspace_root(),
                &workspace_namespace(&registry_ref.namespace),
                task_scope_id.as_str(),
                &line,
            ) {
                tracing::warn!(?error, "append routing binding coordination log");
            }
        }
        if nomination
            .swarm
            .as_ref()
            .is_some_and(|s| matches!(s.routing.routing_tier, RoutingTier::L2 | RoutingTier::L3))
            && nomination.grant.is_none()
        {
            let label = nomination
                .swarm
                .as_ref()
                .expect("tier branch implies swarm exists")
                .routing
                .routing_tier
                .to_string();
            let note = if matches!(registry_ref.workspace_phase, WorkspacePhase::Planning) {
                format!(
                    "[routing {label}] task-scoped path: routing this broadcast to planner drafting"
                )
            } else if matches!(
                registry_ref.workspace_phase,
                WorkspacePhase::Assignment | WorkspacePhase::Execution
            ) && registry_ref
                .task_plan
                .work_items
                .iter()
                .any(|item| item.lifecycle == WorkItemLifecycleState::Planned)
            {
                format!(
                    "[routing {label}] task-scoped path: running assignment for planned work items"
                )
            } else if matches!(
                registry_ref.workspace_phase,
                WorkspacePhase::Assignment | WorkspacePhase::Execution
            ) {
                format!(
                    "[routing {label}] task-scoped path: no planned work items to assign — add work items in the plan or return to planning"
                )
            } else {
                format!(
                    "[routing {label}] skipped message-level nomination — use planner / work items"
                )
            };
            for window in &mut registry_ref.windows {
                window.transcript_lines.push(note.clone());
            }
            match registry_ref.workspace_phase {
                WorkspacePhase::Planning => {
                    steer_planner_nl_followup = Some(message.body.clone());
                }
                WorkspacePhase::Assignment | WorkspacePhase::Execution => {
                    let has_planned = registry_ref
                        .task_plan
                        .work_items
                        .iter()
                        .any(|item| item.lifecycle == WorkItemLifecycleState::Planned);
                    if has_planned {
                        assignment_routed_followup = true;
                    }
                }
                _ => {}
            }
        }
        if let Some(grant) = nomination.grant {
            let speaker_name = registry_ref
                .windows
                .iter()
                .find(|window| window.instance_id == grant.instance_id)
                .map(|window| window.instance_name.clone())
                .unwrap_or_else(|| grant.instance_id.clone());
            let round = grant.round;
            for window in &mut registry_ref.windows {
                window.transcript_lines.push(format!(
                    "[nomination] {speaker_name} won speaking rights for {} in round {round}",
                    grant.message_id
                ));
            }
            let winning_agent = agents
                .iter()
                .find(|agent| agent.binding.instance_id == grant.instance_id);
            if let Some(responder) =
                winning_agent.and_then(|agent| agent.binding.responder.as_ref())
            {
                if responder.is_human() {
                    let request = orchestrator.build_reply_request_for_grant(
                        &registry_ref.runtime,
                        &agents,
                        &grant,
                    )?;
                    queue_human_reply(&mut registry_ref, request)?;
                    for window in &mut registry_ref.windows {
                        window
                            .transcript_lines
                            .push(format!("[pending] {} will reply manually", speaker_name));
                    }
                } else {
                    let reply_backend = default_reply_backend();
                    match orchestrator.generate_and_post_reply(
                        &reply_backend,
                        &mut registry_ref.runtime,
                        &agents,
                        &grant,
                    ) {
                        Ok(reply) => {
                            let recipient_name = registry_ref
                                .windows
                                .iter()
                                .find(|window| window.instance_id == reply.from)
                                .map(|window| window.instance_name.clone())
                                .unwrap_or_else(|| reply.from.clone());
                            let target_id = match &reply.route {
                                MessageRoute::Direct { to } => to.clone(),
                                _ => String::new(),
                            };
                            let target_name = registry_ref
                                .windows
                                .iter()
                                .find(|window| window.instance_id == target_id)
                                .map(|window| window.instance_name.clone())
                                .unwrap_or(target_id);
                            for window in &mut registry_ref.windows {
                                if window.instance_id == reply.from {
                                    window.transcript_lines.push(format!(
                                        "[reply {}] you -> {}: {}",
                                        reply.id, target_name, reply.body
                                    ));
                                } else if matches!(&reply.route, MessageRoute::Direct { to } if to == &window.instance_id)
                                {
                                    window.transcript_lines.push(format!(
                                        "[recv {}] {} -> you: {}",
                                        reply.id, recipient_name, reply.body
                                    ));
                                }
                            }
                        }
                        Err(error) => {
                            for window in &mut registry_ref.windows {
                                window.transcript_lines.push(format!(
                                    "[llm error] nomination winner {} could not reply: {error}",
                                    speaker_name
                                ));
                            }
                        }
                    }
                }
            }
        }
    }

    sync_windows(registry);
    if let Some(planner_line) = steer_planner_nl_followup {
        if let Err(error) = run_planner_natural_language_input(registry, &planner_line, false) {
            tracing::warn!(
                ?error,
                "L2/L3 broadcast: planner drafting from chat failed after routing"
            );
        }
    }
    if assignment_routed_followup {
        if let Err(error) = maybe_auto_assign_on_task_routed_chat(registry) {
            tracing::warn!(
                ?error,
                "L2/L3 broadcast: auto-assign / execute after task routing failed"
            );
        }
        sync_windows(registry);
    }
    Ok(())
}

fn create_channel(
    registry: &Rc<RefCell<UiRegistry>>,
    window_index: i32,
    channel_name: &str,
) -> Result<()> {
    let created_name = {
        let mut registry_ref = registry.borrow_mut();
        let session_id = registry_ref.session_id.clone();
        let result = registry_ref
            .runtime
            .dispatch(RuntimeCommand::CreateChannel {
                session_id,
                name: channel_name.to_owned(),
            })?;
        let RuntimeCommandResult::Channel(channel) = result else {
            bail!("unexpected runtime result while creating channel");
        };
        channel.name
    };
    append_local_line(
        registry,
        window_index,
        format!("created channel #{created_name}"),
    );
    sync_windows(registry);
    Ok(())
}

fn join_channel(
    registry: &Rc<RefCell<UiRegistry>>,
    window_index: i32,
    channel_name: &str,
) -> Result<()> {
    let joined_name = {
        let mut registry_ref = registry.borrow_mut();
        let target_index = registry_ref
            .windows
            .iter()
            .position(|window| window.window_index == window_index)
            .ok_or_else(|| anyhow::anyhow!("window not found: {window_index}"))?;
        let instance_id = registry_ref.windows[target_index].instance_id.clone();
        let channel_id = resolve_channel_name(
            &registry_ref.runtime,
            &registry_ref.session_id,
            channel_name,
        )?;
        let result = registry_ref.runtime.dispatch(RuntimeCommand::JoinChannel {
            instance_id,
            channel_id: channel_id.clone(),
        })?;
        let RuntimeCommandResult::Instance(_) = result else {
            bail!("unexpected runtime result while joining channel");
        };
        registry_ref
            .runtime
            .state()
            .channels
            .iter()
            .find(|channel| channel.id == channel_id)
            .map(|channel| channel.name.clone())
            .context("channel should exist after join")?
    };
    append_local_line(registry, window_index, format!("joined #{joined_name}"));
    sync_windows(registry);
    Ok(())
}

fn leave_channel(
    registry: &Rc<RefCell<UiRegistry>>,
    window_index: i32,
    channel_name: &str,
) -> Result<()> {
    let left_name = {
        let mut registry_ref = registry.borrow_mut();
        let target_index = registry_ref
            .windows
            .iter()
            .position(|window| window.window_index == window_index)
            .ok_or_else(|| anyhow::anyhow!("window not found: {window_index}"))?;
        let instance_id = registry_ref.windows[target_index].instance_id.clone();
        let channel_id = resolve_channel_name(
            &registry_ref.runtime,
            &registry_ref.session_id,
            channel_name,
        )?;
        let name = registry_ref
            .runtime
            .state()
            .channels
            .iter()
            .find(|channel| channel.id == channel_id)
            .map(|channel| channel.name.clone())
            .context("channel should exist before leave")?;
        let result = registry_ref
            .runtime
            .dispatch(RuntimeCommand::LeaveChannel {
                instance_id,
                channel_id,
            })?;
        let RuntimeCommandResult::Instance(_) = result else {
            bail!("unexpected runtime result while leaving channel");
        };
        name
    };
    append_local_line(registry, window_index, format!("left #{left_name}"));
    sync_windows(registry);
    Ok(())
}

fn send_channel_message(
    registry: &Rc<RefCell<UiRegistry>>,
    from_window_index: i32,
    channel_name: &str,
    body: &str,
) -> Result<()> {
    let mut steer_planner_nl_followup = None::<String>;
    let mut assignment_routed_followup = false;
    {
        let mut registry_ref = registry.borrow_mut();
        let from_index = registry_ref
            .windows
            .iter()
            .position(|window| window.window_index == from_window_index)
            .ok_or_else(|| anyhow::anyhow!("window not found: {from_window_index}"))?;

        let session_id = registry_ref.session_id.clone();
        let from_id = registry_ref.windows[from_index].instance_id.clone();
        let from_name = registry_ref.windows[from_index].instance_name.clone();
        let channel_id = resolve_channel_name(&registry_ref.runtime, &session_id, channel_name)?;
        let channel_label = registry_ref
            .runtime
            .state()
            .channels
            .iter()
            .find(|channel| channel.id == channel_id)
            .map(|channel| channel.name.clone())
            .context("channel should exist while posting")?;

        let result = registry_ref.runtime.dispatch(RuntimeCommand::PostMessage {
            session_id,
            from: from_id,
            route: MessageRoute::Channel {
                channel_id: channel_id.clone(),
            },
            kind: MessageKind::Chat,
            body: body.to_owned(),
            reply_to: None,
        })?;
        let RuntimeCommandResult::Message(message) = result else {
            bail!("unexpected runtime result while posting channel message");
        };

        let recipients = registry_ref
            .windows
            .iter()
            .map(|window| {
                let receives = registry_ref
                    .runtime
                    .instance(&window.instance_id)
                    .map(|instance| instance.channel_ids.iter().any(|id| id == &channel_id))
                    .unwrap_or(false);
                (window.window_index, receives)
            })
            .collect::<Vec<_>>();

        for window in &mut registry_ref.windows {
            let receives = recipients
                .iter()
                .find(|(index, _)| *index == window.window_index)
                .map(|(_, receives)| *receives)
                .unwrap_or(false);
            if !receives {
                continue;
            }

            if window.window_index == from_window_index {
                window
                    .transcript_lines
                    .push(format!("[you -> #{}] {}", channel_label, message.body));
            } else {
                window.transcript_lines.push(format!(
                    "[channel {}] {} -> #{}: {}",
                    message.id, from_name, channel_label, message.body
                ));
            }
        }

        let orchestrator = registry_ref.orchestrator.clone();
        let agents = registry_ref.agents.clone();
        let conversation_active = registry_ref.conversation_active_task_id.clone();
        let task_scope_id = registry_ref.task_id.clone();
        let ts = current_timestamp_ms();
        let nomination = orchestrator.run_nomination_cycle(
            &mut registry_ref.runtime,
            &agents,
            &message,
            ts,
            conversation_active.as_deref(),
            Some(task_scope_id.as_str()),
        )?;
        if let Some(ref swarm) = nomination.swarm {
            registry_ref.conversation_active_task_id = swarm.task_binding.active_task_id.clone();

            if swarm.task_binding.task_binding_action == TaskBindingAction::CreateImplicitTask {
                let key = ImplicitIntentDedupeKey::from_trigger(
                    message.session_id.clone(),
                    message.id.clone(),
                    &message.body,
                );
                if registry_ref.implicit_intent_seen.contains(&key) {
                    tracing::info!(
                        message_id = %message.id,
                        session_id = %message.session_id,
                        "implicit work intent dedupe: duplicate ADR-003 key, skip journal append"
                    );
                } else {
                    let record = ImplicitIntentDedupeRecord::from_key(&key, ts);
                    if let Err(error) = append_implicit_intent_dedupe_record(
                        hc_bootstrap::workspace_root(),
                        &workspace_namespace(&registry_ref.namespace),
                        task_scope_id.as_str(),
                        &record,
                    ) {
                        tracing::warn!(?error, "append implicit intent dedupe record");
                    } else {
                        registry_ref.implicit_intent_seen.insert(key);
                    }
                }
            }

            let line =
                build_routing_binding_log_line_v1(ts, &message, task_scope_id.as_str(), swarm);
            if let Err(error) = append_routing_binding_log_line(
                hc_bootstrap::workspace_root(),
                &workspace_namespace(&registry_ref.namespace),
                task_scope_id.as_str(),
                &line,
            ) {
                tracing::warn!(?error, "append routing binding coordination log");
            }
        }
        if nomination
            .swarm
            .as_ref()
            .is_some_and(|s| matches!(s.routing.routing_tier, RoutingTier::L2 | RoutingTier::L3))
            && nomination.grant.is_none()
        {
            let label = nomination
                .swarm
                .as_ref()
                .expect("tier branch implies swarm exists")
                .routing
                .routing_tier
                .to_string();
            let note = if matches!(registry_ref.workspace_phase, WorkspacePhase::Planning) {
                format!(
                    "[routing {label}] task-scoped path: routing this channel post to planner drafting"
                )
            } else if matches!(
                registry_ref.workspace_phase,
                WorkspacePhase::Assignment | WorkspacePhase::Execution
            ) && registry_ref
                .task_plan
                .work_items
                .iter()
                .any(|item| item.lifecycle == WorkItemLifecycleState::Planned)
            {
                format!(
                    "[routing {label}] task-scoped path: running assignment for planned work items"
                )
            } else if matches!(
                registry_ref.workspace_phase,
                WorkspacePhase::Assignment | WorkspacePhase::Execution
            ) {
                format!(
                    "[routing {label}] task-scoped path: no planned work items to assign — add work items in the plan or return to planning"
                )
            } else {
                format!(
                    "[routing {label}] skipped message-level nomination — use planner / work items"
                )
            };
            for window in &mut registry_ref.windows {
                window.transcript_lines.push(note.clone());
            }
            match registry_ref.workspace_phase {
                WorkspacePhase::Planning => {
                    steer_planner_nl_followup = Some(message.body.clone());
                }
                WorkspacePhase::Assignment | WorkspacePhase::Execution => {
                    let has_planned = registry_ref
                        .task_plan
                        .work_items
                        .iter()
                        .any(|item| item.lifecycle == WorkItemLifecycleState::Planned);
                    if has_planned {
                        assignment_routed_followup = true;
                    }
                }
                _ => {}
            }
        }
        if let Some(grant) = nomination.grant {
            let speaker_name = registry_ref
                .windows
                .iter()
                .find(|window| window.instance_id == grant.instance_id)
                .map(|window| window.instance_name.clone())
                .unwrap_or_else(|| grant.instance_id.clone());
            for window in &mut registry_ref.windows {
                window.transcript_lines.push(format!(
                    "[nomination] {speaker_name} won speaking rights for {}",
                    grant.message_id
                ));
            }
            let winning_agent = agents
                .iter()
                .find(|agent| agent.binding.instance_id == grant.instance_id);
            if let Some(responder) =
                winning_agent.and_then(|agent| agent.binding.responder.as_ref())
            {
                if responder.is_human() {
                    let request = orchestrator.build_reply_request_for_grant(
                        &registry_ref.runtime,
                        &agents,
                        &grant,
                    )?;
                    queue_human_reply(&mut registry_ref, request)?;
                    for window in &mut registry_ref.windows {
                        window
                            .transcript_lines
                            .push(format!("[pending] {} will reply manually", speaker_name));
                    }
                } else {
                    let reply_backend = default_reply_backend();
                    match orchestrator.generate_and_post_reply(
                        &reply_backend,
                        &mut registry_ref.runtime,
                        &agents,
                        &grant,
                    ) {
                        Ok(reply) => {
                            let target_id = match &reply.route {
                                MessageRoute::Direct { to } => to.clone(),
                                _ => String::new(),
                            };
                            let target_name = registry_ref
                                .windows
                                .iter()
                                .find(|window| window.instance_id == target_id)
                                .map(|window| window.instance_name.clone())
                                .unwrap_or(target_id);
                            for window in &mut registry_ref.windows {
                                if window.instance_id == reply.from {
                                    window.transcript_lines.push(format!(
                                        "[reply {}] you -> {}: {}",
                                        reply.id, target_name, reply.body
                                    ));
                                } else if matches!(&reply.route, MessageRoute::Direct { to } if to == &window.instance_id)
                                {
                                    window.transcript_lines.push(format!(
                                        "[recv {}] {} -> you: {}",
                                        reply.id, speaker_name, reply.body
                                    ));
                                }
                            }
                        }
                        Err(error) => {
                            for window in &mut registry_ref.windows {
                                window.transcript_lines.push(format!(
                                    "[llm error] nomination winner {} could not reply: {error}",
                                    speaker_name
                                ));
                            }
                        }
                    }
                }
            }
        }
    }

    sync_windows(registry);
    if let Some(planner_line) = steer_planner_nl_followup {
        if let Err(error) = run_planner_natural_language_input(registry, &planner_line, false) {
            tracing::warn!(
                ?error,
                "L2/L3 channel chat: planner drafting failed after routing"
            );
        }
    }
    if assignment_routed_followup {
        if let Err(error) = maybe_auto_assign_on_task_routed_chat(registry) {
            tracing::warn!(
                ?error,
                "L2/L3 channel chat: auto-assign / execute after task routing failed"
            );
        }
        sync_windows(registry);
    }
    Ok(())
}

fn list_window_channels(
    registry: &Rc<RefCell<UiRegistry>>,
    window_index: i32,
) -> Result<Vec<String>> {
    let registry_ref = registry.borrow();
    let window = registry_ref
        .windows
        .iter()
        .find(|window| window.window_index == window_index)
        .ok_or_else(|| anyhow::anyhow!("window not found: {window_index}"))?;
    let instance = registry_ref
        .runtime
        .instance(&window.instance_id)
        .context("instance should exist")?;

    if instance.channel_ids.is_empty() {
        return Ok(vec!["no joined channels".to_owned()]);
    }

    Ok(instance
        .channel_ids
        .iter()
        .map(|channel_id| {
            registry_ref
                .runtime
                .state()
                .channels
                .iter()
                .find(|channel| channel.id == *channel_id)
                .map(|channel| format!("#{} ({})", channel.name, channel.id))
                .unwrap_or_else(|| channel_id.clone())
        })
        .collect())
}

fn list_pending_replies(
    registry: &Rc<RefCell<UiRegistry>>,
    window_index: i32,
) -> Result<Vec<String>> {
    let registry_ref = registry.borrow();
    let window = registry_ref
        .windows
        .iter()
        .find(|window| window.window_index == window_index)
        .ok_or_else(|| anyhow::anyhow!("window not found: {window_index}"))?;

    if window.pending_replies.is_empty() {
        return Ok(vec!["no pending replies".to_owned()]);
    }

    Ok(window
        .pending_replies
        .iter()
        .enumerate()
        .map(|(index, request)| {
            let sender_name = registry_ref
                .windows
                .iter()
                .find(|candidate| candidate.instance_id == request.source_from_instance_id)
                .map(|candidate| candidate.instance_name.clone())
                .unwrap_or_else(|| request.source_from_instance_id.clone());
            format!(
                "{}. from {} [{}]: {}",
                index + 1,
                sender_name,
                request.source_message_id,
                request.source_body
            )
        })
        .collect())
}

fn resolve_channel_name(
    runtime: &RuntimeSupervisor,
    session_id: &str,
    selector: &str,
) -> Result<String> {
    runtime
        .state()
        .channels
        .iter()
        .find(|channel| {
            channel.session_id == session_id
                && (channel.id == selector
                    || channel.name == selector
                    || format!("#{}", channel.name) == selector)
        })
        .map(|channel| channel.id.clone())
        .ok_or_else(|| anyhow::anyhow!("channel not found: {selector}"))
}

fn run_local_command_async(registry: Rc<RefCell<UiRegistry>>, window_index: i32, line: String) {
    let sender = registry.borrow().events_tx.clone();

    thread::spawn(move || {
        let spawn_result = if cfg!(windows) {
            Command::new("powershell")
                .args(["-NoProfile", "-Command", &line])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
        } else {
            Command::new("sh")
                .arg("-c")
                .arg(&line)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
        };

        let mut child = match spawn_result {
            Ok(child) => child,
            Err(error) => {
                let _ = sender.send(UiEvent::CommandOutput {
                    window_index,
                    line: format!("failed to run command: {error}"),
                });
                return;
            }
        };

        let stdout = child.stdout.take();
        let stderr = child.stderr.take();

        if let Some(stdout) = stdout {
            let sender = sender.clone();
            let _ = thread::spawn(move || {
                stream_output_lines(stdout, window_index, sender, false);
            });
        }

        if let Some(stderr) = stderr {
            let sender = sender.clone();
            let _ = thread::spawn(move || {
                stream_output_lines(stderr, window_index, sender, true);
            });
        }

        match child.wait() {
            Ok(status) if status.success() => {
                let _ = sender.send(UiEvent::CommandExit {
                    window_index,
                    status: "command completed".to_owned(),
                });
            }
            Ok(status) => {
                let _ = sender.send(UiEvent::CommandExit {
                    window_index,
                    status: format!("command exited with {status}"),
                });
            }
            Err(error) => {
                let _ = sender.send(UiEvent::CommandExit {
                    window_index,
                    status: format!("failed to wait for command: {error}"),
                });
            }
        }
    });
}

fn stream_output_lines<R: Read + Send + 'static>(
    reader: R,
    window_index: i32,
    sender: Sender<UiEvent>,
    is_stderr: bool,
) {
    let mut reader = BufReader::new(reader);
    let mut buffer = Vec::new();

    loop {
        buffer.clear();
        match reader.read_until(b'\n', &mut buffer) {
            Ok(0) => break,
            Ok(_) => {
                let mut line = String::from_utf8_lossy(&buffer).replace('\r', "");
                if line.ends_with('\n') {
                    line.pop();
                }
                if is_stderr {
                    line = format!("stderr: {line}");
                }
                let _ = sender.send(UiEvent::CommandOutput { window_index, line });
            }
            Err(error) => {
                let prefix = if is_stderr { "stderr" } else { "stdout" };
                let _ = sender.send(UiEvent::CommandOutput {
                    window_index,
                    line: format!("{prefix} read error: {error}"),
                });
                break;
            }
        }
    }
}

fn pump_ui_events(registry: &Rc<RefCell<UiRegistry>>) {
    let mut changed = false;
    if sync_external_human_replies(registry).unwrap_or(false) {
        changed = true;
    }

    loop {
        let event = {
            let registry_ref = registry.borrow();
            registry_ref.events_rx.try_recv().ok()
        };

        let Some(event) = event else {
            break;
        };

        {
            let mut registry_ref = registry.borrow_mut();
            match event {
                UiEvent::CommandOutput { window_index, line } => {
                    if let Some(window) = registry_ref
                        .windows
                        .iter_mut()
                        .find(|window| window.window_index == window_index)
                    {
                        window.transcript_lines.push(line);
                        changed = true;
                    }
                }
                UiEvent::CommandExit {
                    window_index,
                    status,
                } => {
                    if let Some(window) = registry_ref
                        .windows
                        .iter_mut()
                        .find(|window| window.window_index == window_index)
                    {
                        window.transcript_lines.push(status);
                        changed = true;
                    }
                }
            }
        }
    }

    if changed {
        sync_windows(registry);
    }
}

fn sync_external_human_replies(registry: &Rc<RefCell<UiRegistry>>) -> Result<bool> {
    let answered_items = {
        let registry_ref = registry.borrow();
        let repository = HumanInboxRepository::with_namespace(
            hc_bootstrap::workspace_root(),
            hc_store::store::WorkspaceNamespace::new(
                registry_ref.namespace.tenant_id.clone(),
                registry_ref.namespace.user_id.clone(),
            ),
        );
        repository.list_answered()?
    };

    if answered_items.is_empty() {
        return Ok(false);
    }

    let mut changed = false;
    let repository = {
        let registry_ref = registry.borrow();
        HumanInboxRepository::with_namespace(
            hc_bootstrap::workspace_root(),
            hc_store::store::WorkspaceNamespace::new(
                registry_ref.namespace.tenant_id.clone(),
                registry_ref.namespace.user_id.clone(),
            ),
        )
    };

    for item in answered_items {
        let Some(response_body) = item.response_body.clone() else {
            continue;
        };
        let mut registry_ref = registry.borrow_mut();
        let result = registry_ref.runtime.dispatch(RuntimeCommand::PostMessage {
            session_id: item.source_session_id.clone(),
            from: item.replying_instance_id.clone(),
            route: MessageRoute::Direct {
                to: item.source_from_instance_id.clone(),
            },
            kind: MessageKind::Chat,
            body: response_body.clone(),
            reply_to: Some(item.source_message_id.clone()),
        })?;
        let RuntimeCommandResult::Message(reply) = result else {
            bail!("unexpected runtime result while syncing answered human reply");
        };

        for window in &mut registry_ref.windows {
            if window.instance_id == item.replying_instance_id {
                window.pending_replies.retain(|request| {
                    !(request.source_message_id == item.source_message_id
                        && request.replying_instance_id == item.replying_instance_id)
                });
                window.transcript_lines.push(format!(
                    "[manual reply {}] you -> {}: {}",
                    reply.id, item.source_from_instance_id, reply.body
                ));
            } else if window.instance_id == item.source_from_instance_id {
                window.transcript_lines.push(format!(
                    "[recv {}] {} -> you: {}",
                    reply.id, item.replying_agent_name, reply.body
                ));
            }
        }
        drop(registry_ref);
        repository.mark_completed(&item.id)?;
        changed = true;
    }

    Ok(changed)
}

fn sync_windows(registry: &Rc<RefCell<UiRegistry>>) {
    let (titles, snapshots) = {
        let registry_ref = registry.borrow();
        let workspace_view = current_workspace_view(&registry_ref);
        let persisted_artifacts = registry_ref.task_artifacts.clone();
        let titles = registry_ref
            .windows
            .iter()
            .map(|window| {
                SharedString::from(format!("{} [{}]", window.instance_name, window.role_name))
            })
            .collect::<Vec<_>>();

        let snapshots = registry_ref
            .windows
            .iter()
            .map(|window| {
                let transcript_text = render_window_transcript(
                    &workspace_view,
                    &persisted_artifacts,
                    &window.instance_name,
                    &window.role_name,
                    window.focused_target.as_deref(),
                    &window.transcript_lines,
                );
                let agent_board_text = render_agent_board(&workspace_view);
                let agent_slots = render_agent_slots(&workspace_view);
                let work_items_text = render_work_queue(&workspace_view);
                let work_item_slots = render_work_item_slots(&workspace_view);
                let inspector_text = render_inspector(
                    &workspace_view,
                    &persisted_artifacts,
                    &window.instance_id,
                    &window.instance_name,
                    window.focused_target.as_deref(),
                );
                (
                    window.handle.clone_strong(),
                    render_prompt_label(&workspace_view, window),
                    agent_board_text,
                    agent_slots[0].clone(),
                    agent_slots[1].clone(),
                    agent_slots[2].clone(),
                    agent_slots[3].clone(),
                    agent_slots[4].clone(),
                    agent_slots[5].clone(),
                    work_items_text,
                    work_item_slots[0].clone(),
                    work_item_slots[1].clone(),
                    work_item_slots[2].clone(),
                    work_item_slots[3].clone(),
                    work_item_slots[4].clone(),
                    work_item_slots[5].clone(),
                    window
                        .selected_work_item
                        .clone()
                        .map(|id| format!("Selected work item: {id}"))
                        .unwrap_or_else(|| "Selected work item: none".to_owned()),
                    inspector_text,
                    transcript_text,
                )
            })
            .collect::<Vec<_>>();

        (titles, snapshots)
    };

    let model = ModelRc::new(VecModel::from(titles));
    for (
        handle,
        prompt_label,
        agent_board_text,
        agent_slot_1,
        agent_slot_2,
        agent_slot_3,
        agent_slot_4,
        agent_slot_5,
        agent_slot_6,
        work_items_text,
        work_item_slot_1,
        work_item_slot_2,
        work_item_slot_3,
        work_item_slot_4,
        work_item_slot_5,
        work_item_slot_6,
        selected_work_item_label,
        inspector_text,
        transcript_text,
    ) in snapshots
    {
        handle.set_open_window_titles(model.clone());
        handle.set_prompt_label(prompt_label.into());
        handle.set_agent_board_text(agent_board_text.into());
        handle.set_agent_slot_1(agent_slot_1.into());
        handle.set_agent_slot_2(agent_slot_2.into());
        handle.set_agent_slot_3(agent_slot_3.into());
        handle.set_agent_slot_4(agent_slot_4.into());
        handle.set_agent_slot_5(agent_slot_5.into());
        handle.set_agent_slot_6(agent_slot_6.into());
        handle.set_work_items_text(work_items_text.into());
        handle.set_work_item_slot_1(work_item_slot_1.into());
        handle.set_work_item_slot_2(work_item_slot_2.into());
        handle.set_work_item_slot_3(work_item_slot_3.into());
        handle.set_work_item_slot_4(work_item_slot_4.into());
        handle.set_work_item_slot_5(work_item_slot_5.into());
        handle.set_work_item_slot_6(work_item_slot_6.into());
        handle.set_selected_work_item_label(selected_work_item_label.into());
        handle.set_inspector_text(inspector_text.into());
        handle.set_transcript_text(transcript_text.into());
    }
}

fn current_workspace_view(registry: &UiRegistry) -> hc_agent::WorkspaceViewModel {
    let task = TaskRequest::new(
        registry.task_id.clone(),
        registry.task_title.clone(),
        registry.task_goal.clone(),
    )
    .with_namespace(TaskNamespace::new(
        registry.namespace.tenant_id.clone(),
        registry.namespace.user_id.clone(),
    ));
    let session = registry
        .runtime
        .session(&registry.session_id)
        .cloned()
        .unwrap_or_else(|| SessionRecord {
            id: registry.session_id.clone(),
            name: registry.task_title.clone(),
            namespace: registry.namespace.clone(),
            instance_ids: Vec::new(),
            channel_ids: Vec::new(),
        });
    let workbench = AgentWorkbench {
        task,
        session,
        task_plan: registry.task_plan.clone(),
        plan: AgentPlan {
            task_id: registry.task_id.clone(),
            namespace: TaskNamespace::new(
                registry.namespace.tenant_id.clone(),
                registry.namespace.user_id.clone(),
            ),
            seeds: Vec::new(),
        },
        phase: registry.workspace_phase.clone(),
        agents: registry.agents.clone(),
        channel_conversations: Vec::new(),
    };
    let mut view = build_workspace_view(&registry.runtime, &workbench);
    for card in &mut view.agent_cards {
        if let Some(window) = registry
            .windows
            .iter()
            .find(|window| window.instance_id == card.instance_id)
        {
            card.pending_reply_count = window.pending_replies.len();
        }
    }
    view
}

fn current_task_request(registry: &UiRegistry) -> TaskRequest {
    TaskRequest::new(
        registry.task_id.clone(),
        registry.task_title.clone(),
        registry.task_goal.clone(),
    )
    .with_namespace(TaskNamespace::new(
        registry.namespace.tenant_id.clone(),
        registry.namespace.user_id.clone(),
    ))
    .with_budget(TaskBudget {
        token_budget: registry.task_plan.task_token_budget,
        time_budget_minutes: registry.task_plan.task_time_budget_minutes,
        evolution_reserve_tokens: registry.task_plan.evolution_reserve_tokens,
    })
}

fn refresh_persisted_task_artifacts(registry: &mut UiRegistry) -> Result<()> {
    let task = current_task_request(registry);
    let namespace = hc_store::store::WorkspaceNamespace::new(
        registry.namespace.tenant_id.clone(),
        registry.namespace.user_id.clone(),
    );
    persist_task_artifacts_with_in_memory_prune(
        hc_bootstrap::workspace_root(),
        &task,
        &mut registry.task_plan,
    )?;
    registry.task_artifacts = query_task_artifacts(
        hc_bootstrap::workspace_root(),
        &namespace,
        &hc_agent::TaskArtifactQuery::default().for_task(registry.task_id.clone()),
    )?;
    Ok(())
}

fn ui_store_namespace(registry: &UiRegistry) -> hc_store::store::WorkspaceNamespace {
    hc_store::store::WorkspaceNamespace::new(
        registry.namespace.tenant_id.clone(),
        registry.namespace.user_id.clone(),
    )
}

fn append_work_item_claim_journal_for_id(registry: &UiRegistry, claim_id: &str) -> Result<()> {
    let claim = registry
        .task_plan
        .work_item_claims
        .iter()
        .find(|row| row.id == claim_id)
        .ok_or_else(|| anyhow::anyhow!("missing work item claim row: {claim_id}"))?;
    append_work_item_claim_journal_line(
        hc_bootstrap::workspace_root(),
        &ui_store_namespace(registry),
        &registry.task_id,
        claim,
    )
    .map(|_| ())
}

fn append_work_item_claims_journal_for_work_item(
    registry: &UiRegistry,
    work_item_id: &str,
) -> Result<()> {
    let root = hc_bootstrap::workspace_root();
    let namespace = ui_store_namespace(registry);
    for claim in registry
        .task_plan
        .work_item_claims
        .iter()
        .filter(|row| row.work_item_id == work_item_id)
    {
        append_work_item_claim_journal_line(&root, &namespace, &registry.task_id, claim)?;
    }
    Ok(())
}

fn append_work_item_assignment_journal_for_id(
    registry: &UiRegistry,
    assignment_id: &str,
) -> Result<()> {
    let assignment = registry
        .task_plan
        .work_item_assignments
        .iter()
        .find(|row| row.id == assignment_id)
        .ok_or_else(|| anyhow::anyhow!("missing work item assignment row: {assignment_id}"))?;
    append_work_item_assignment_journal_line(
        hc_bootstrap::workspace_root(),
        &ui_store_namespace(registry),
        &registry.task_id,
        assignment,
    )
    .map(|_| ())
}

fn append_journal_for_executing_assignment(
    registry: &UiRegistry,
    work_item_id: &str,
) -> Result<()> {
    let assignment = registry
        .task_plan
        .work_item_assignments
        .iter()
        .find(|row| row.work_item_id == work_item_id && row.status == "executing");
    let Some(assignment) = assignment else {
        return Ok(());
    };
    append_work_item_assignment_journal_line(
        hc_bootstrap::workspace_root(),
        &ui_store_namespace(registry),
        &registry.task_id,
        assignment,
    )
    .map(|_| ())
}

fn render_window_transcript(
    workspace: &hc_agent::WorkspaceViewModel,
    persisted_artifacts: &[TaskArtifactSummary],
    instance_name: &str,
    role_name: &str,
    focused_target: Option<&str>,
    transcript_lines: &[String],
) -> String {
    let mut lines = vec![
        format!("Task: {}", workspace.task_title),
        format!("Goal: {}", workspace.task_goal),
        format!("Phase: {}", workspace.phase),
        format!(
            "Plan: {} | Work items: {} | Agent proposals: {} | Claims: {} | Assignments: {}",
            workspace.plan_status,
            workspace.work_item_count,
            workspace.proposed_agent_count,
            workspace.work_item_claim_count,
            workspace.work_item_assignment_count
        ),
        format!("Target: {}", focused_target.unwrap_or("workspace")),
        format!(
            "Agent: {} ({}) | Team: {}",
            instance_name,
            role_name,
            workspace
                .agent_cards
                .iter()
                .map(|card| format!("{}:{}", card.name, card.status))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        "Recent Activity:".to_owned(),
    ];

    if workspace.recent_activity.is_empty() {
        lines.push("- none".to_owned());
    } else {
        lines.extend(
            workspace
                .recent_activity
                .iter()
                .take(4)
                .map(render_activity_line),
        );
    }

    if !workspace.decision_traces.is_empty() {
        lines.push(String::new());
        lines.push("Decision Trace:".to_owned());
        lines.extend(
            workspace
                .decision_traces
                .iter()
                .take(4)
                .map(render_decision_trace_line),
        );
    }

    if !workspace.planning_notes.is_empty() {
        lines.push(String::new());
        lines.push("Planning Notes:".to_owned());
        lines.extend(
            workspace
                .planning_notes
                .iter()
                .map(|note| format!("- {note}")),
        );
    }

    lines.push(String::new());
    lines.push("Work Items:".to_owned());
    lines.extend(workspace.work_item_lines.iter().cloned());

    lines.push(String::new());
    lines.push("Assignments:".to_owned());
    lines.extend(workspace.assignment_lines.iter().cloned());

    lines.push(String::new());
    lines.push("Persisted Task Assets:".to_owned());
    lines.extend(render_persisted_task_artifact_lines(persisted_artifacts, 4));

    lines.push(String::new());
    if let Some(card) = workspace
        .agent_cards
        .iter()
        .find(|card| card.name == instance_name)
    {
        lines.push(format!(
            "Responder: {} | Pending: {}",
            card.responder_label
                .clone()
                .unwrap_or_else(|| "not bound".to_owned()),
            card.pending_reply_count
        ));
        lines.push(format!(
            "Agent Code: {} | Behavior Mode: {}",
            card.agent_code, card.behavior_mode_code
        ));
        lines.push(String::new());
    }
    lines.push("Thread:".to_owned());
    lines.extend_from_slice(transcript_lines);
    lines.join("\n")
}

fn render_activity_line(activity: &ActivityItemView) -> String {
    format!(
        "- [{}:{}:{}] {} :: {}",
        activity.kind, activity.stage, activity.actor, activity.title, activity.detail
    )
}

fn render_decision_trace_line(trace: &hc_agent::DecisionTraceView) -> String {
    format!(
        "- [{}:{}] {} => {} :: {}",
        trace.code, trace.stage, trace.subject, trace.outcome, trace.detail
    )
}

fn render_agent_board(workspace: &hc_agent::WorkspaceViewModel) -> String {
    let mut lines = vec![
        format!("Task: {}", workspace.task_title),
        format!("Namespace: {}", workspace.namespace_label),
        String::new(),
    ];

    if workspace.agent_cards.is_empty() {
        lines.push("No agents".to_owned());
    } else {
        lines.extend(workspace.agent_cards.iter().map(|card| {
            let capabilities = if card.capability_names.is_empty() {
                "no capabilities".to_owned()
            } else {
                card.capability_names.join(", ")
            };
            let responder = card
                .responder_label
                .clone()
                .unwrap_or_else(|| "none".to_owned());
            format!(
                "{} ({})\nagent: {}\nmode: {}\nstatus: {}\nresponder: {}\ncapabilities: {}\n",
                card.name,
                card.role,
                card.agent_code,
                card.behavior_mode_code,
                card.status,
                responder,
                capabilities
            )
        }));
    }

    lines.join("\n")
}

fn render_work_queue(workspace: &hc_agent::WorkspaceViewModel) -> String {
    let mut lines = vec![
        format!("Phase: {}", workspace.phase),
        format!("Plan: {}", workspace.plan_status),
        String::new(),
        "Work Items".to_owned(),
    ];

    lines.extend(workspace.work_item_lines.iter().cloned());
    lines.push(String::new());
    lines.push("Assignments".to_owned());
    lines.extend(workspace.assignment_lines.iter().cloned());

    lines.join("\n")
}

fn render_agent_slots(workspace: &hc_agent::WorkspaceViewModel) -> [String; 6] {
    let names = workspace
        .agent_cards
        .iter()
        .map(|card| card.name.clone())
        .take(6)
        .collect::<Vec<_>>();

    [
        names.first().cloned().unwrap_or_default(),
        names.get(1).cloned().unwrap_or_default(),
        names.get(2).cloned().unwrap_or_default(),
        names.get(3).cloned().unwrap_or_default(),
        names.get(4).cloned().unwrap_or_default(),
        names.get(5).cloned().unwrap_or_default(),
    ]
}

fn render_work_item_slots(workspace: &hc_agent::WorkspaceViewModel) -> [String; 6] {
    let ids = workspace
        .work_item_lines
        .iter()
        .filter_map(|line| {
            let trimmed = line.strip_prefix("- ")?;
            let (id, _) = trimmed.split_once(' ')?;
            if id.starts_with("work-item.") {
                Some(id.to_owned())
            } else {
                None
            }
        })
        .take(6)
        .collect::<Vec<_>>();

    [
        ids.first().cloned().unwrap_or_default(),
        ids.get(1).cloned().unwrap_or_default(),
        ids.get(2).cloned().unwrap_or_default(),
        ids.get(3).cloned().unwrap_or_default(),
        ids.get(4).cloned().unwrap_or_default(),
        ids.get(5).cloned().unwrap_or_default(),
    ]
}

fn render_inspector(
    workspace: &hc_agent::WorkspaceViewModel,
    persisted_artifacts: &[TaskArtifactSummary],
    instance_id: &str,
    instance_name: &str,
    focused_target: Option<&str>,
) -> String {
    let card = workspace
        .agent_cards
        .iter()
        .find(|card| card.instance_id == instance_id);

    let mut lines = vec![
        format!("Selected: {}", instance_name),
        format!("Session: {}", workspace.session_id),
        format!("Current target: {}", focused_target.unwrap_or("workspace")),
        format!("Plan status: {}", workspace.plan_status),
        format!(
            "Work items: {} | Agent proposals: {}",
            workspace.work_item_count, workspace.proposed_agent_count
        ),
        format!(
            "Claims: {} | Assignments: {}",
            workspace.work_item_claim_count, workspace.work_item_assignment_count
        ),
        format!(
            "Pending replies: {}",
            workspace
                .agent_cards
                .iter()
                .find(|card| card.instance_id == instance_id)
                .map(|card| card.pending_reply_count)
                .unwrap_or(0)
        ),
        format!("Assets: personas={}", workspace.asset_summary.personas),
        format!(
            "Capabilities: {} | Memory: {}",
            workspace.asset_summary.capabilities, workspace.asset_summary.memory_records
        ),
        String::new(),
    ];

    if let Some(card) = card {
        lines.push(format!("Role: {}", card.role));
        lines.push(format!("Agent code: {}", card.agent_code));
        lines.push(format!("Status: {}", card.status));
        lines.push(format!("Behavior mode: {}", card.behavior_mode_code));
        lines.push(format!(
            "Responder: {}",
            card.responder_label
                .clone()
                .unwrap_or_else(|| "not bound".to_owned())
        ));
        lines.push(format!(
            "Memory scopes: {}",
            if card.memory_scope_refs.is_empty() {
                "none".to_owned()
            } else {
                card.memory_scope_refs.join(", ")
            }
        ));
        lines.push(String::new());
    }

    lines.push("Recent activity:".to_owned());
    if workspace.recent_activity.is_empty() {
        lines.push("- none".to_owned());
    } else {
        lines.extend(
            workspace
                .recent_activity
                .iter()
                .take(3)
                .map(render_activity_line),
        );
    }

    lines.push(String::new());
    lines.push("Planning notes:".to_owned());
    if workspace.planning_notes.is_empty() {
        lines.push("- none".to_owned());
    } else {
        lines.extend(
            workspace
                .planning_notes
                .iter()
                .take(4)
                .map(|note| format!("- {note}")),
        );
    }

    lines.push(String::new());
    lines.push("Work items:".to_owned());
    lines.extend(workspace.work_item_lines.iter().take(4).cloned());

    lines.push(String::new());
    lines.push("Assignments:".to_owned());
    lines.extend(workspace.assignment_lines.iter().take(4).cloned());

    lines.push(String::new());
    lines.push("Decision trace:".to_owned());
    if workspace.decision_traces.is_empty() {
        lines.push("- none".to_owned());
    } else {
        lines.extend(
            workspace
                .decision_traces
                .iter()
                .take(5)
                .map(render_decision_trace_line),
        );
    }

    lines.push(String::new());
    lines.push("Persisted task assets:".to_owned());
    lines.extend(render_persisted_task_artifact_lines(persisted_artifacts, 5));

    lines.join("\n")
}

fn render_persisted_task_artifact_lines(
    persisted_artifacts: &[TaskArtifactSummary],
    limit: usize,
) -> Vec<String> {
    if persisted_artifacts.is_empty() {
        return vec!["- none".to_owned()];
    }

    persisted_artifacts
        .iter()
        .take(limit)
        .map(|artifact| {
            let task_hint = artifact
                .task_hint
                .clone()
                .unwrap_or_else(|| "unknown-task".to_owned());
            format!(
                "- [{}:{}] {} | {} | {}",
                match artifact.kind {
                    hc_agent::TaskArtifactKind::TaskPlan => "task_plan",
                    hc_agent::TaskArtifactKind::AssignmentDecision => "assignment_decision",
                },
                artifact.status,
                task_hint,
                artifact.title,
                artifact.relative_path
            )
        })
        .collect()
}

fn render_prompt_label(
    workspace: &hc_agent::WorkspaceViewModel,
    window: &WindowController,
) -> String {
    let responder = workspace
        .agent_cards
        .iter()
        .find(|card| card.instance_id == window.instance_id)
        .and_then(|card| card.responder_label.clone())
        .unwrap_or_else(|| "none".to_owned());
    let pending = workspace
        .agent_cards
        .iter()
        .find(|card| card.instance_id == window.instance_id)
        .map(|card| card.pending_reply_count)
        .unwrap_or(0);
    format!(
        "{} | {} | phase {} | responder {} | pending {} | target {} | work {}",
        workspace.task_title,
        window.instance_name,
        workspace.phase,
        responder,
        pending,
        window.focused_target.as_deref().unwrap_or("workspace"),
        window.selected_work_item.as_deref().unwrap_or("none")
    )
}

fn configure_default_responder(agent: &mut MaterializedAgent) {
    let responder = preferred_default_responder(&agent.persona.name, &agent.persona.role);
    let binding_ref = match &responder {
        ResponderBinding::Llm(_) => "responder.default.llm",
        ResponderBinding::Human(_) => "responder.default.human",
        ResponderBinding::Rule(_) => "responder.default.rule",
        ResponderBinding::Script(_) => "responder.default.script",
    };
    agent.binding.responder_binding_ref = Some(binding_ref.to_owned());
    agent.binding.responder = Some(responder);
}

fn preferred_default_responder(agent_name: &str, role_name: &str) -> ResponderBinding {
    if has_configured_llm_provider() {
        ResponderBinding::Llm(LlmResponderConfig {
            provider: default_provider_from_env(),
            model: default_model_from_env(),
            system_prompt: render_agent_responder_system_prompt(
                &runtime_namespace(),
                agent_name,
                role_name,
                "Stay concise and collaborative.",
            )
            .ok(),
        })
    } else {
        ResponderBinding::Human(HumanResponderConfig::new(Some(user_id_from_env()), None))
    }
}

fn has_configured_llm_provider() -> bool {
    provider_api_key_from_env(&default_provider_from_env()).is_some()
}

fn append_local_line(registry: &Rc<RefCell<UiRegistry>>, window_index: i32, line: String) {
    let mut registry_ref = registry.borrow_mut();
    if let Some(window) = registry_ref
        .windows
        .iter_mut()
        .find(|window| window.window_index == window_index)
    {
        window.transcript_lines.push(line);
    }
}

fn add_plan_note(registry: &Rc<RefCell<UiRegistry>>, window_index: i32, note: &str) -> Result<()> {
    let mut registry_ref = registry.borrow_mut();
    registry_ref.task_plan.add_note(note.to_owned());
    if let Some(window) = registry_ref
        .windows
        .iter_mut()
        .find(|window| window.window_index == window_index)
    {
        window
            .transcript_lines
            .push(format!("[plan] note added: {note}"));
    }
    refresh_persisted_task_artifacts(&mut registry_ref)?;
    drop(registry_ref);
    sync_windows(registry);
    Ok(())
}

fn add_plan_stage(
    registry: &Rc<RefCell<UiRegistry>>,
    window_index: i32,
    stage: &str,
    title: &str,
    goal: &str,
) -> Result<()> {
    let mut registry_ref = registry.borrow_mut();
    let work_item_id =
        registry_ref
            .task_plan
            .add_work_item(stage.to_owned(), title.to_owned(), goal.to_owned());
    if let Some(window) = registry_ref
        .windows
        .iter_mut()
        .find(|window| window.window_index == window_index)
    {
        window.transcript_lines.push(format!(
            "[plan] stage added {} | {} | {} | {}",
            work_item_id, stage, title, goal
        ));
    }
    refresh_persisted_task_artifacts(&mut registry_ref)?;
    drop(registry_ref);
    sync_windows(registry);
    Ok(())
}

fn add_plan_agent_proposal(
    registry: &Rc<RefCell<UiRegistry>>,
    window_index: i32,
    role: &str,
    reason: &str,
) -> Result<()> {
    let mut registry_ref = registry.borrow_mut();
    let proposal_id = registry_ref
        .task_plan
        .add_agent_proposal(role.to_owned(), reason.to_owned());
    if let Some(window) = registry_ref
        .windows
        .iter_mut()
        .find(|window| window.window_index == window_index)
    {
        window.transcript_lines.push(format!(
            "[plan] agent proposal added {} | {} | {}",
            proposal_id, role, reason
        ));
    }
    refresh_persisted_task_artifacts(&mut registry_ref)?;
    drop(registry_ref);
    sync_windows(registry);
    Ok(())
}

fn approve_plan(registry: &Rc<RefCell<UiRegistry>>, window_index: i32) -> Result<()> {
    let mut registry_ref = registry.borrow_mut();
    registry_ref.task_plan.approve();
    registry_ref.workspace_phase = WorkspacePhase::Assignment;

    let proposed_roles = registry_ref
        .task_plan
        .agent_proposals
        .iter_mut()
        .filter(|proposal| proposal.status == "proposed")
        .map(|proposal| {
            proposal.status = "materialized".to_owned();
            (
                proposal.id.clone(),
                proposal.role.clone(),
                proposal.reason.clone(),
            )
        })
        .collect::<Vec<_>>();

    let session_id = registry_ref.session_id.clone();
    let task_id = registry_ref.task_id.clone();
    let namespace = TaskNamespace::new(
        registry_ref.namespace.tenant_id.clone(),
        registry_ref.namespace.user_id.clone(),
    );

    let mut created = Vec::new();
    for (proposal_id, role, reason) in proposed_roles {
        let proposed_name = next_agent_name_for_role(&registry_ref.agents, &role);
        let seed = AgentSeed::new(
            format!("{}.{}", task_id, proposal_id),
            proposed_name.clone(),
            role.clone(),
            reason.clone(),
        );
        let mut agent = materialize_seed(
            &mut registry_ref.runtime,
            &session_id,
            &task_id,
            &namespace,
            &seed,
        )?;
        configure_default_responder(&mut agent);
        created.push(agent.persona.name.clone());
        registry_ref.agents.push(agent);
    }

    if let Some(window) = registry_ref
        .windows
        .iter_mut()
        .find(|window| window.window_index == window_index)
    {
        window.transcript_lines.push("[plan] approved".to_owned());
        if created.is_empty() {
            window
                .transcript_lines
                .push("[assignment] no new agents created from plan proposals".to_owned());
        } else {
            window.transcript_lines.push(format!(
                "[assignment] materialized agents: {}",
                created.join(", ")
            ));
        }
    }
    refresh_persisted_task_artifacts(&mut registry_ref)?;
    drop(registry_ref);
    sync_windows(registry);
    Ok(())
}

fn add_work_item_claim(
    registry: &Rc<RefCell<UiRegistry>>,
    window_index: i32,
    work_item_id: &str,
    agent_name: &str,
    score: f32,
    reason: &str,
) -> Result<()> {
    let mut registry_ref = registry.borrow_mut();
    let Some(agent) = registry_ref
        .agents
        .iter()
        .find(|agent| agent.persona.name == agent_name)
        .cloned()
    else {
        bail!("unknown agent: {agent_name}");
    };

    let claim_id = registry_ref.task_plan.add_work_item_claim(
        work_item_id.to_owned(),
        agent.binding.instance_id.clone(),
        agent.persona.name.clone(),
        score,
        reason.to_owned(),
    );

    if let Some(window) = registry_ref
        .windows
        .iter_mut()
        .find(|window| window.window_index == window_index)
    {
        window.transcript_lines.push(format!(
            "[assignment] claim added {} | work item {} | {} | score {:.2} | {}",
            claim_id, work_item_id, agent_name, score, reason
        ));
    }

    append_work_item_claim_journal_for_id(&registry_ref, &claim_id)?;

    refresh_persisted_task_artifacts(&mut registry_ref)?;
    drop(registry_ref);
    sync_windows(registry);
    Ok(())
}

fn resolve_work_item_assignment(
    registry: &Rc<RefCell<UiRegistry>>,
    window_index: i32,
    work_item_id: &str,
) -> Result<()> {
    let mut registry_ref = registry.borrow_mut();
    let assignment_id = registry_ref
        .task_plan
        .resolve_work_item_assignment(work_item_id)
        .ok_or_else(|| anyhow::anyhow!("no submitted claims for work item: {work_item_id}"))?;
    let assignment = registry_ref
        .task_plan
        .work_item_assignments
        .iter()
        .find(|assignment| assignment.id == assignment_id)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("assignment not found after resolve: {assignment_id}"))?;

    if let Some(window) = registry_ref
        .windows
        .iter_mut()
        .find(|window| window.window_index == window_index)
    {
        window.transcript_lines.push(format!(
            "[assignment] resolved {} | work item {} -> {} ({})",
            assignment.id, assignment.work_item_id, assignment.agent_name, assignment.rationale
        ));
    }

    append_work_item_claims_journal_for_work_item(&registry_ref, work_item_id)?;
    append_work_item_assignment_journal_for_id(&registry_ref, &assignment_id)?;

    refresh_persisted_task_artifacts(&mut registry_ref)?;
    drop(registry_ref);
    sync_windows(registry);
    Ok(())
}

fn set_selected_work_item(
    registry: &Rc<RefCell<UiRegistry>>,
    window_index: i32,
    work_item_id: String,
) -> Result<()> {
    let mut registry_ref = registry.borrow_mut();
    if !registry_ref
        .task_plan
        .work_items
        .iter()
        .any(|item| item.id == work_item_id)
    {
        bail!("unknown work item: {work_item_id}");
    }

    let window = registry_ref
        .windows
        .iter_mut()
        .find(|window| window.window_index == window_index)
        .ok_or_else(|| anyhow::anyhow!("window not found: {window_index}"))?;
    window.selected_work_item = Some(work_item_id.clone());
    window
        .transcript_lines
        .push(format!("[work-item] selected {work_item_id}"));
    drop(registry_ref);
    sync_windows(registry);
    Ok(())
}

fn claim_selected_work_item(
    registry: &Rc<RefCell<UiRegistry>>,
    window_index: i32,
    score_text: String,
    reason_text: String,
) -> Result<()> {
    let score = score_text
        .trim()
        .parse::<f32>()
        .map_err(|_| anyhow::anyhow!("invalid claim score: {}", score_text.trim()))?;
    let (work_item_id, agent_name) = {
        let registry_ref = registry.borrow();
        let window = registry_ref
            .windows
            .iter()
            .find(|window| window.window_index == window_index)
            .ok_or_else(|| anyhow::anyhow!("window not found: {window_index}"))?;
        let work_item_id = window
            .selected_work_item
            .clone()
            .ok_or_else(|| anyhow::anyhow!("no work item selected"))?;
        (work_item_id, window.instance_name.clone())
    };
    add_work_item_claim(
        registry,
        window_index,
        &work_item_id,
        &agent_name,
        score,
        reason_text.trim(),
    )
}

fn clear_window_focus_target(registry: &Rc<RefCell<UiRegistry>>, window_index: i32) -> Result<()> {
    let mut registry_ref = registry.borrow_mut();
    let window = registry_ref
        .windows
        .iter_mut()
        .find(|window| window.window_index == window_index)
        .ok_or_else(|| anyhow::anyhow!("window not found: {window_index}"))?;
    window.focused_target = None;
    window
        .transcript_lines
        .push("[focus] target reset to workspace".to_owned());
    drop(registry_ref);
    sync_windows(registry);
    Ok(())
}

fn run_planner_natural_language_input(
    registry: &Rc<RefCell<UiRegistry>>,
    input: &str,
    initial: bool,
) -> Result<()> {
    let request = {
        let registry_ref = registry.borrow();
        let planner = registry_ref
            .agents
            .iter()
            .find(|agent| agent.persona.role == "planner")
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("planner agent not found"))?;
        let responder = planner
            .binding
            .responder
            .clone()
            .ok_or_else(|| anyhow::anyhow!("planner responder not bound"))?;

        if matches!(&responder, ResponderBinding::Human(_)) {
            drop(registry_ref);
            return apply_manual_planner_input(registry, input, initial, responder.label());
        }

        let source_body = render_agent_planner_input_prompt_body(
            &registry_ref.namespace,
            &registry_ref.task_title,
            &registry_ref.task_goal,
            &format!("{:?}", registry_ref.task_plan.status),
            &registry_ref.task_plan.planning_notes.join("\n"),
            &registry_ref
                .task_plan
                .work_items
                .iter()
                .map(|item| format!("{} | {} | {}", item.stage, item.title, item.goal))
                .collect::<Vec<_>>()
                .join("\n"),
            &registry_ref
                .task_plan
                .agent_proposals
                .iter()
                .map(|proposal| format!("{} | {}", proposal.role, proposal.reason))
                .collect::<Vec<_>>()
                .join("\n"),
            input.trim(),
        )?;

        ReplyRequest {
            source_message_id: if initial {
                "planner-input.initial".to_owned()
            } else {
                format!("planner-input.{}", current_timestamp_ms())
            },
            source_session_id: registry_ref.session_id.clone(),
            source_from_instance_id: "user.workspace".to_owned(),
            source_body,
            replying_instance_id: planner.binding.instance_id.clone(),
            replying_agent_name: planner.persona.name.clone(),
            replying_role: planner.persona.role.clone(),
            responder,
        }
    };

    let backend = default_reply_backend();
    let response = backend.generate_reply(&request)?;
    let draft = parse_planner_draft(&response.body)?;
    apply_planner_draft(registry, &draft, input, initial)
}

fn apply_manual_planner_input(
    registry: &Rc<RefCell<UiRegistry>>,
    input: &str,
    initial: bool,
    responder_label: String,
) -> Result<()> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Ok(());
    }

    let mut registry_ref = registry.borrow_mut();
    registry_ref.task_plan.add_note(trimmed.to_owned());
    if let Some(window) = registry_ref
        .windows
        .iter_mut()
        .find(|window| window.role_name == "planner")
    {
        if initial {
            window.transcript_lines.push(format!(
                "[planning] task captured. Planner is {} so the initial request was stored as a planning note.",
                responder_label
            ));
        } else {
            window.transcript_lines.push(format!(
                "[planning] note recorded from natural language input: {}",
                trimmed
            ));
        }
        window.transcript_lines.push(
            "[planning] Next: keep describing stages, goals, and needed roles in plain language, or switch planner to an llm responder for automatic structuring."
                .to_owned(),
        );
    }
    let planning_counts = format!(
        "[planning] notes now: {} | work items: {} | agent proposals: {}",
        registry_ref.task_plan.planning_notes.len(),
        registry_ref.task_plan.work_items.len(),
        registry_ref.task_plan.agent_proposals.len()
    );
    for window in &mut registry_ref.windows {
        window.transcript_lines.push(planning_counts.clone());
    }
    refresh_persisted_task_artifacts(&mut registry_ref)?;
    drop(registry_ref);
    sync_windows(registry);
    Ok(())
}

fn parse_planner_draft(body: &str) -> Result<PlannerDraft> {
    let trimmed = body.trim();
    serde_json::from_str::<PlannerDraft>(trimmed)
        .map_err(|error| anyhow::anyhow!("planner returned invalid structured plan: {error}"))
}

fn apply_planner_draft(
    registry: &Rc<RefCell<UiRegistry>>,
    draft: &PlannerDraft,
    input: &str,
    initial: bool,
) -> Result<()> {
    let mut registry_ref = registry.borrow_mut();
    if draft.notes.is_empty() && draft.work_items.is_empty() && draft.agent_proposals.is_empty() {
        bail!("planner returned an empty draft");
    }

    if let Some(window) = registry_ref
        .windows
        .iter_mut()
        .find(|window| window.role_name == "planner")
    {
        if initial {
            window.transcript_lines.push(format!(
                "[planning] planner initialized from task: {}",
                input.trim()
            ));
        } else {
            window
                .transcript_lines
                .push(format!("[planning] planner input: {}", input.trim()));
        }
    }

    for note in &draft.notes {
        registry_ref.task_plan.add_note(note.clone());
    }
    for item in &draft.work_items {
        registry_ref.task_plan.add_work_item(
            item.stage.clone(),
            item.title.clone(),
            item.goal.clone(),
        );
    }
    for proposal in &draft.agent_proposals {
        registry_ref
            .task_plan
            .add_agent_proposal(proposal.role.clone(), proposal.reason.clone());
    }

    let summary = format!(
        "[planning] drafted {} notes, {} work items, {} agent proposals",
        draft.notes.len(),
        draft.work_items.len(),
        draft.agent_proposals.len()
    );
    for window in &mut registry_ref.windows {
        window.transcript_lines.push(summary.clone());
    }

    if matches!(registry_ref.workspace_phase, WorkspacePhase::Planning)
        && !draft.work_items.is_empty()
    {
        registry_ref.task_plan.approve();
        registry_ref.workspace_phase = WorkspacePhase::Assignment;

        let proposed_roles = registry_ref
            .task_plan
            .agent_proposals
            .iter_mut()
            .filter(|proposal| proposal.status == "proposed")
            .map(|proposal| {
                proposal.status = "materialized".to_owned();
                (
                    proposal.id.clone(),
                    proposal.role.clone(),
                    proposal.reason.clone(),
                )
            })
            .collect::<Vec<_>>();

        let session_id = registry_ref.session_id.clone();
        let task_id = registry_ref.task_id.clone();
        let namespace = TaskNamespace::new(
            registry_ref.namespace.tenant_id.clone(),
            registry_ref.namespace.user_id.clone(),
        );

        let mut created = Vec::new();
        for (proposal_id, role, reason) in proposed_roles {
            let proposed_name = next_agent_name_for_role(&registry_ref.agents, &role);
            let seed = AgentSeed::new(
                format!("{}.{}", task_id, proposal_id),
                proposed_name.clone(),
                role.clone(),
                reason.clone(),
            );
            let mut agent = materialize_seed(
                &mut registry_ref.runtime,
                &session_id,
                &task_id,
                &namespace,
                &seed,
            )?;
            configure_default_responder(&mut agent);
            created.push(agent.persona.name.clone());
            registry_ref.agents.push(agent);
        }

        let transition = if created.is_empty() {
            "[assignment] plan auto-approved; no new agents were required".to_owned()
        } else {
            format!(
                "[assignment] plan auto-approved; materialized agents: {}",
                created.join(", ")
            )
        };
        for window in &mut registry_ref.windows {
            window.transcript_lines.push(transition.clone());
        }

        auto_assign_and_execute(&mut registry_ref)?;
    }

    refresh_persisted_task_artifacts(&mut registry_ref)?;
    drop(registry_ref);
    sync_windows(registry);
    Ok(())
}

fn maybe_auto_assign_on_task_routed_chat(registry: &Rc<RefCell<UiRegistry>>) -> Result<()> {
    let mut registry_ref = registry.borrow_mut();
    auto_assign_and_execute(&mut registry_ref)
}

fn auto_assign_and_execute(registry_ref: &mut UiRegistry) -> Result<()> {
    let work_item_ids = registry_ref
        .task_plan
        .work_items
        .iter()
        .filter(|item| item.lifecycle == WorkItemLifecycleState::Planned)
        .map(|item| item.id.clone())
        .collect::<Vec<_>>();

    for work_item_id in work_item_ids {
        let Some(work_item) = registry_ref
            .task_plan
            .work_items
            .iter()
            .find(|item| item.id == work_item_id)
            .cloned()
        else {
            continue;
        };

        let best = registry_ref
            .agents
            .iter()
            .map(|agent| {
                let score = auto_assignment_score(
                    &agent.persona.role,
                    &format!("{} {} {}", work_item.stage, work_item.title, work_item.goal),
                );
                (agent.clone(), score)
            })
            .filter(|(_, score)| *score >= 0.55)
            .max_by(|(_, left), (_, right)| left.total_cmp(right));

        let Some((agent, score)) = best else {
            for window in &mut registry_ref.windows {
                window.transcript_lines.push(format!(
                    "[assignment] {} has no confident agent match yet; manual guidance needed",
                    work_item.id
                ));
            }
            continue;
        };

        let reason = format!("auto match from role {}", agent.persona.role);
        let claim_id = registry_ref.task_plan.add_work_item_claim(
            work_item.id.clone(),
            agent.binding.instance_id.clone(),
            agent.persona.name.clone(),
            score,
            reason.clone(),
        );
        append_work_item_claim_journal_for_id(&registry_ref, &claim_id)?;
        let assignment_id = registry_ref
            .task_plan
            .resolve_work_item_assignment(&work_item.id)
            .ok_or_else(|| anyhow::anyhow!("auto assignment failed for {}", work_item.id))?;
        append_work_item_claims_journal_for_work_item(&registry_ref, &work_item.id)?;
        append_work_item_assignment_journal_for_id(&registry_ref, &assignment_id)?;
        let assignment = registry_ref
            .task_plan
            .work_item_assignments
            .iter()
            .find(|assignment| assignment.id == assignment_id)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("assignment missing after auto resolve"))?;

        for window in &mut registry_ref.windows {
            window.transcript_lines.push(format!(
                "[assignment] auto claim {} | {} | {:.2} | {}",
                claim_id, work_item.id, score, reason
            ));
            window.transcript_lines.push(format!(
                "[assignment] auto resolved {} | {} -> {}",
                assignment.id, assignment.work_item_id, assignment.agent_name
            ));
        }
    }

    let executable_ids = registry_ref
        .task_plan
        .work_items
        .iter()
        .filter(|item| item.lifecycle == WorkItemLifecycleState::Assigned)
        .map(|item| item.id.clone())
        .collect::<Vec<_>>();

    if !executable_ids.is_empty() {
        registry_ref.workspace_phase = WorkspacePhase::Execution;
    }

    for work_item_id in executable_ids {
        auto_execute_work_item(registry_ref, &work_item_id)?;
    }

    refresh_persisted_task_artifacts(registry_ref)?;
    Ok(())
}

fn auto_execute_work_item(registry_ref: &mut UiRegistry, work_item_id: &str) -> Result<()> {
    let source_index = registry_ref
        .windows
        .iter()
        .position(|window| window.role_name == "planner")
        .unwrap_or(0);
    let source_id = registry_ref.windows[source_index].instance_id.clone();
    let source_name = registry_ref.windows[source_index].instance_name.clone();
    let session_id = registry_ref.session_id.clone();

    let assigned_agent_id = registry_ref
        .task_plan
        .start_work_item_execution(work_item_id)
        .ok_or_else(|| anyhow::anyhow!("work item is not ready for execution: {work_item_id}"))?;
    append_journal_for_executing_assignment(&*registry_ref, work_item_id)?;
    let work_item = registry_ref
        .task_plan
        .work_items
        .iter()
        .find(|item| item.id == work_item_id)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("work item not found after execution start"))?;
    let assigned_agent = registry_ref
        .agents
        .iter()
        .find(|agent| agent.binding.instance_id == assigned_agent_id)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("assigned agent not found: {assigned_agent_id}"))?;
    let target_name = assigned_agent.persona.name.clone();

    let execution_prompt = render_agent_work_item_execution_prompt_body(
        &registry_ref.namespace,
        &work_item.id,
        &work_item.stage,
        &work_item.title,
        &work_item.goal,
    )?;
    let result = registry_ref.runtime.dispatch(RuntimeCommand::PostMessage {
        session_id,
        from: source_id.clone(),
        route: MessageRoute::Direct {
            to: assigned_agent_id.clone(),
        },
        kind: MessageKind::Chat,
        body: execution_prompt,
        reply_to: None,
    })?;
    let RuntimeCommandResult::Message(message) = result else {
        bail!("unexpected runtime result while auto-starting execution");
    };

    registry_ref.windows[source_index]
        .transcript_lines
        .push(format!(
            "[execution] auto-started {} -> {} ({})",
            work_item.id, target_name, work_item.title
        ));
    registry_ref.windows[source_index]
        .transcript_lines
        .push(format!("[you -> {target_name}] {}", message.body));

    if let Some(target_window) = registry_ref
        .windows
        .iter_mut()
        .find(|window| window.instance_id == assigned_agent_id)
    {
        target_window.transcript_lines.push(format!(
            "[execute {}] {} -> you: {}",
            message.id, source_name, message.body
        ));
    }

    let orchestrator = registry_ref.orchestrator.clone();
    let agents = registry_ref.agents.clone();
    if let Some(responder) = assigned_agent.binding.responder.as_ref() {
        if responder.is_human() {
            let request = orchestrator.build_direct_reply_request(
                &registry_ref.runtime,
                &agents,
                &message.id,
                &assigned_agent_id,
            )?;
            queue_human_reply(registry_ref, request)?;
            registry_ref.windows[source_index]
                .transcript_lines
                .push(format!("[pending] {target_name} will execute manually"));
        } else {
            let reply_backend = default_reply_backend();
            match orchestrator.generate_and_post_direct_reply(
                &reply_backend,
                &mut registry_ref.runtime,
                &agents,
                &message.id,
                &assigned_agent_id,
            ) {
                Ok(reply) => {
                    if let Some(target_window) = registry_ref
                        .windows
                        .iter_mut()
                        .find(|window| window.instance_id == assigned_agent_id)
                    {
                        target_window.transcript_lines.push(format!(
                            "[reply {}] you -> {}: {}",
                            reply.id, source_name, reply.body
                        ));
                    }
                    registry_ref.windows[source_index]
                        .transcript_lines
                        .push(format!(
                            "[recv {}] {} -> you: {}",
                            reply.id, target_name, reply.body
                        ));
                }
                Err(error) => {
                    registry_ref.windows[source_index]
                        .transcript_lines
                        .push(format!(
                            "[llm error] {target_name} could not execute: {error}"
                        ));
                }
            }
        }
    } else {
        registry_ref.windows[source_index]
            .transcript_lines
            .push(format!(
                "[responder] {} has no responder bound",
                target_name
            ));
    }

    refresh_persisted_task_artifacts(registry_ref)?;
    Ok(())
}

fn auto_assignment_score(role: &str, text: &str) -> f32 {
    let lower = text.to_lowercase();
    match role {
        "planner" => {
            if contains_any(
                &lower,
                &["plan", "stage", "phase", "roadmap", "analyze", "??", "??"],
            ) {
                0.88
            } else {
                0.22
            }
        }
        "worker" => {
            if contains_any(
                &lower,
                &[
                    "implement",
                    "build",
                    "write",
                    "code",
                    "fix",
                    "??",
                    "??",
                    "??",
                    "??",
                ],
            ) {
                0.90
            } else {
                0.30
            }
        }
        "reviewer" => {
            if contains_any(
                &lower,
                &[
                    "review", "check", "verify", "risk", "audit", "??", "??", "??",
                ],
            ) {
                0.89
            } else {
                0.28
            }
        }
        _ => 0.20,
    }
}

fn contains_any(body: &str, keywords: &[&str]) -> bool {
    keywords.iter().any(|keyword| body.contains(keyword))
}

fn resolve_selected_work_item(registry: &Rc<RefCell<UiRegistry>>, window_index: i32) -> Result<()> {
    let work_item_id = {
        let registry_ref = registry.borrow();
        registry_ref
            .windows
            .iter()
            .find(|window| window.window_index == window_index)
            .and_then(|window| window.selected_work_item.clone())
            .ok_or_else(|| anyhow::anyhow!("no work item selected"))?
    };
    resolve_work_item_assignment(registry, window_index, &work_item_id)
}

fn execute_selected_work_item(registry: &Rc<RefCell<UiRegistry>>, window_index: i32) -> Result<()> {
    let work_item_id = {
        let registry_ref = registry.borrow();
        registry_ref
            .windows
            .iter()
            .find(|window| window.window_index == window_index)
            .and_then(|window| window.selected_work_item.clone())
            .ok_or_else(|| anyhow::anyhow!("no work item selected"))?
    };
    execute_work_item(registry, window_index, &work_item_id)
}

fn execute_work_item(
    registry: &Rc<RefCell<UiRegistry>>,
    window_index: i32,
    work_item_id: &str,
) -> Result<()> {
    {
        let mut registry_ref = registry.borrow_mut();
        let source_index = registry_ref
            .windows
            .iter()
            .position(|window| window.window_index == window_index)
            .ok_or_else(|| anyhow::anyhow!("window not found: {window_index}"))?;
        let session_id = registry_ref.session_id.clone();
        let source_id = registry_ref.windows[source_index].instance_id.clone();
        let source_name = registry_ref.windows[source_index].instance_name.clone();

        let assigned_agent_id = registry_ref
            .task_plan
            .start_work_item_execution(work_item_id)
            .ok_or_else(|| anyhow::anyhow!("work item is not in assigned state: {work_item_id}"))?;
        append_journal_for_executing_assignment(&*registry_ref, work_item_id)?;
        registry_ref.workspace_phase = WorkspacePhase::Execution;

        let work_item = registry_ref
            .task_plan
            .work_items
            .iter()
            .find(|item| item.id == work_item_id)
            .cloned()
            .ok_or_else(|| {
                anyhow::anyhow!("work item not found after execution start: {work_item_id}")
            })?;
        let assigned_agent = registry_ref
            .agents
            .iter()
            .find(|agent| agent.binding.instance_id == assigned_agent_id)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("assigned agent not found: {assigned_agent_id}"))?;
        let target_name = assigned_agent.persona.name.clone();

        let execution_prompt = render_agent_work_item_execution_prompt_body(
            &registry_ref.namespace,
            &work_item.id,
            &work_item.stage,
            &work_item.title,
            &work_item.goal,
        )?;

        let result = registry_ref.runtime.dispatch(RuntimeCommand::PostMessage {
            session_id,
            from: source_id.clone(),
            route: MessageRoute::Direct {
                to: assigned_agent_id.clone(),
            },
            kind: MessageKind::Chat,
            body: execution_prompt,
            reply_to: None,
        })?;
        let RuntimeCommandResult::Message(message) = result else {
            bail!("unexpected runtime result while starting execution");
        };

        registry_ref.windows[source_index]
            .transcript_lines
            .push(format!(
                "[execution] started {} -> {} ({})",
                work_item.id, target_name, work_item.title
            ));
        registry_ref.windows[source_index]
            .transcript_lines
            .push(format!("[you -> {target_name}] {}", message.body));

        if let Some(target_window) = registry_ref
            .windows
            .iter_mut()
            .find(|window| window.instance_id == assigned_agent_id)
        {
            target_window.transcript_lines.push(format!(
                "[execute {}] {} -> you: {}",
                message.id, source_name, message.body
            ));
        }

        let orchestrator = registry_ref.orchestrator.clone();
        let agents = registry_ref.agents.clone();
        if let Some(responder) = assigned_agent.binding.responder.as_ref() {
            if responder.is_human() {
                let request = orchestrator.build_direct_reply_request(
                    &registry_ref.runtime,
                    &agents,
                    &message.id,
                    &assigned_agent_id,
                )?;
                queue_human_reply(&mut registry_ref, request)?;
                registry_ref.windows[source_index]
                    .transcript_lines
                    .push(format!("[pending] {target_name} will execute manually"));
            } else {
                let reply_backend = default_reply_backend();
                match orchestrator.generate_and_post_direct_reply(
                    &reply_backend,
                    &mut registry_ref.runtime,
                    &agents,
                    &message.id,
                    &assigned_agent_id,
                ) {
                    Ok(reply) => {
                        if let Some(target_window) = registry_ref
                            .windows
                            .iter_mut()
                            .find(|window| window.instance_id == assigned_agent_id)
                        {
                            target_window.transcript_lines.push(format!(
                                "[reply {}] you -> {}: {}",
                                reply.id, source_name, reply.body
                            ));
                        }
                        registry_ref.windows[source_index]
                            .transcript_lines
                            .push(format!(
                                "[recv {}] {} -> you: {}",
                                reply.id, target_name, reply.body
                            ));
                    }
                    Err(error) => {
                        registry_ref.windows[source_index]
                            .transcript_lines
                            .push(format!(
                                "[llm error] {target_name} could not execute: {error}"
                            ));
                    }
                }
            }
        } else {
            registry_ref.windows[source_index]
                .transcript_lines
                .push(format!(
                    "[responder] {} has no responder bound",
                    target_name
                ));
        }

        refresh_persisted_task_artifacts(&mut registry_ref)?;
    }

    sync_windows(registry);
    Ok(())
}

fn next_agent_name_for_role(agents: &[MaterializedAgent], role: &str) -> String {
    let count = agents
        .iter()
        .filter(|agent| agent.persona.role == role)
        .count();
    if count == 0 {
        role.to_owned()
    } else {
        format!("{role}-{}", count + 1)
    }
}

fn parse_msg_command(line: &str) -> Option<(String, String)> {
    let rest = line.strip_prefix("/msg ")?;
    let mut parts = rest.splitn(2, ' ');
    let target = parts.next()?.trim();
    let body = parts.next()?.trim();
    if target.is_empty() || body.is_empty() {
        return None;
    }
    Some((target.to_owned(), body.to_owned()))
}

fn parse_targeted_message(line: &str) -> Option<(String, String)> {
    let rest = line.strip_prefix('@')?;
    let mut parts = rest.splitn(2, ' ');
    let target = parts.next()?.trim();
    let body = parts.next()?.trim();
    if target.is_empty() || body.is_empty() {
        return None;
    }
    Some((target.to_owned(), body.to_owned()))
}

fn parse_name_command(line: &str) -> Option<String> {
    let rest = line.strip_prefix("/name ")?;
    let name = rest.trim();
    if name.is_empty() {
        return None;
    }
    Some(name.to_owned())
}

fn parse_focus_command(line: &str) -> Option<String> {
    let rest = line.strip_prefix("/focus ")?;
    let name = rest.trim();
    if name.is_empty() {
        return None;
    }
    Some(name.to_owned())
}

fn parse_responder_command(line: &str) -> Option<String> {
    let rest = line.strip_prefix("/responder ")?;
    let mode = rest.trim();
    if mode.is_empty() {
        return None;
    }
    Some(mode.to_owned())
}

fn parse_reply_command(line: &str) -> Option<String> {
    let rest = line.strip_prefix("/reply ")?;
    let body = rest.trim();
    if body.is_empty() {
        return None;
    }
    Some(body.to_owned())
}

fn parse_run_command(line: &str) -> Option<String> {
    let rest = line.strip_prefix("/run ")?;
    let command = rest.trim();
    if command.is_empty() {
        return None;
    }
    Some(command.to_owned())
}

fn parse_all_command(line: &str) -> Option<String> {
    let rest = line.strip_prefix("/all ")?;
    let body = rest.trim();
    if body.is_empty() {
        return None;
    }
    Some(body.to_owned())
}

fn parse_channel_message_command(line: &str) -> Option<(String, String)> {
    let rest = line.strip_prefix("/channel ")?;
    let mut parts = rest.splitn(2, ' ');
    let channel = parts.next()?.trim();
    let body = parts.next()?.trim();
    if channel.is_empty() || body.is_empty() {
        return None;
    }
    Some((channel.to_owned(), body.to_owned()))
}

fn parse_channel_create_command(line: &str) -> Option<String> {
    let rest = line.strip_prefix("/channel-create ")?;
    let name = rest.trim();
    if name.is_empty() {
        return None;
    }
    Some(name.to_owned())
}

fn parse_join_command(line: &str) -> Option<String> {
    let rest = line.strip_prefix("/join ")?;
    let name = rest.trim();
    if name.is_empty() {
        return None;
    }
    Some(name.to_owned())
}

fn parse_leave_command(line: &str) -> Option<String> {
    let rest = line.strip_prefix("/leave ")?;
    let name = rest.trim();
    if name.is_empty() {
        return None;
    }
    Some(name.to_owned())
}

fn parse_plan_note_command(line: &str) -> Option<String> {
    let rest = line.strip_prefix("/plan note ")?;
    let note = rest.trim();
    if note.is_empty() {
        return None;
    }
    Some(note.to_owned())
}

fn parse_plan_stage_command(line: &str) -> Option<(String, String, String)> {
    let rest = line.strip_prefix("/plan stage ")?;
    let parts = rest.split(" | ").map(str::trim).collect::<Vec<_>>();
    if parts.len() != 3 || parts.iter().any(|part| part.is_empty()) {
        return None;
    }
    Some((
        parts[0].to_owned(),
        parts[1].to_owned(),
        parts[2].to_owned(),
    ))
}

fn parse_plan_agent_command(line: &str) -> Option<(String, String)> {
    let rest = line.strip_prefix("/plan agent ")?;
    let parts = rest.split(" | ").map(str::trim).collect::<Vec<_>>();
    if parts.len() != 2 || parts.iter().any(|part| part.is_empty()) {
        return None;
    }
    Some((parts[0].to_owned(), parts[1].to_owned()))
}

fn parse_assign_claim_command(line: &str) -> Option<(String, String, f32, String)> {
    let rest = line.strip_prefix("/assign claim ")?;
    let parts = rest.split(" | ").map(str::trim).collect::<Vec<_>>();
    if parts.len() != 4 || parts.iter().any(|part| part.is_empty()) {
        return None;
    }
    let score = parts[2].parse::<f32>().ok()?;
    Some((
        parts[0].to_owned(),
        parts[1].to_owned(),
        score,
        parts[3].to_owned(),
    ))
}

fn runtime_namespace() -> RuntimeNamespace {
    RuntimeNamespace::new(tenant_id_from_env(), user_id_from_env())
}

fn workspace_namespace(namespace: &RuntimeNamespace) -> hc_store::store::WorkspaceNamespace {
    hc_store::store::WorkspaceNamespace::new(namespace.tenant_id.clone(), namespace.user_id.clone())
}

fn render_agent_responder_system_prompt(
    namespace: &RuntimeNamespace,
    agent_name: &str,
    role_name: &str,
    style: &str,
) -> Result<String> {
    Ok(
        load_agent_responder_system_prompt(&workspace_namespace(namespace))?
            .replace("{{agent_name}}", agent_name)
            .replace("{{role_name}}", role_name)
            .replace("{{style}}", style),
    )
}

fn render_agent_planner_input_prompt_body(
    namespace: &RuntimeNamespace,
    task_title: &str,
    task_goal: &str,
    plan_status: &str,
    planning_notes: &str,
    work_items: &str,
    agent_proposals: &str,
    user_input: &str,
) -> Result<String> {
    Ok(
        load_agent_planner_input_prompt(&workspace_namespace(namespace))?
            .replace("{{task_title}}", task_title)
            .replace("{{task_goal}}", task_goal)
            .replace("{{plan_status}}", plan_status)
            .replace("{{planning_notes}}", planning_notes)
            .replace("{{work_items}}", work_items)
            .replace("{{agent_proposals}}", agent_proposals)
            .replace("{{user_input}}", user_input),
    )
}

fn render_agent_work_item_execution_prompt_body(
    namespace: &RuntimeNamespace,
    work_item_id: &str,
    stage: &str,
    title: &str,
    goal: &str,
) -> Result<String> {
    Ok(
        load_agent_work_item_execution_prompt(&workspace_namespace(namespace))?
            .replace("{{work_item_id}}", work_item_id)
            .replace("{{stage}}", stage)
            .replace("{{title}}", title)
            .replace("{{goal}}", goal),
    )
}

fn summarize_task_title(task_goal: &str) -> String {
    let trimmed = task_goal.trim();
    if trimmed.is_empty() {
        return "Untitled Task".to_owned();
    }

    let mut title = trimmed
        .lines()
        .next()
        .unwrap_or(trimmed)
        .trim()
        .chars()
        .take(48)
        .collect::<String>();
    if trimmed.chars().count() > 48 {
        title.push_str("...");
    }
    title
}

fn current_timestamp_ms() -> u64 {
    hc_bootstrap::wall_clock_ms()
}

fn default_llm_registry() -> ProviderRegistry {
    default_registry_from_env()
}

fn default_reply_backend() -> RegistryReplyBackend {
    RegistryReplyBackend {
        registry: default_llm_registry(),
    }
}
