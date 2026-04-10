use std::{
    cell::RefCell,
    env,
    io::{BufRead, BufReader, Read},
    process::{Command, Stdio},
    rc::Rc,
    sync::mpsc::{self, Receiver, Sender},
    thread,
    time::Duration,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use hc_agent::{
    ActivityItemView, AgentOrchestrator, AgentPlan, AgentSeed, AgentWorkbench, MaterializedAgent,
    TaskNamespace, TaskPlan, TaskRequest, WorkspacePhase, bootstrap_task_workbench,
    build_workspace_view, materialize_seed,
};
use hc_core::{
    MessageKind, MessageRoute, RuntimeCommand, RuntimeCommandResult, RuntimeNamespace, SessionRecord,
    RuntimeSupervisor,
};
use hc_llm::{OpenAiCompatibleProvider, ProviderRegistry};
use hc_responder::{
    HumanInboxRepository, HumanResponderConfig, LlmResponderConfig, ReplyRequest, ReplyResponse,
    ResponderBackend, ResponderBinding, require_human, require_llm,
};
use slint::{ComponentHandle, ModelRc, SharedString, Timer, TimerMode, VecModel, Weak};

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
        in property <string> inspector-text;
        in property <string> transcript-text;
        in property <string> prompt-label;

        callback new_window();
        callback close_window();
        callback submit_input(string);

        title: window-title;
        width: 720px;
        height: 520px;

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
                    placeholder-text: "Ask the workspace, focus an agent, or use /reply /run";
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
    pending_replies: Vec<ReplyRequest>,
    handle: MultiWindowShell,
    transcript_lines: Vec<String>,
}

struct UiRegistry {
    runtime: RuntimeSupervisor,
    orchestrator: AgentOrchestrator,
    session_id: String,
    task_id: String,
    task_title: String,
    task_goal: String,
    task_plan: TaskPlan,
    workspace_phase: WorkspacePhase,
    namespace: RuntimeNamespace,
    agents: Vec<MaterializedAgent>,
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
        let llm = require_llm(&request.responder)?;
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
                *app_registry.borrow_mut() = Some(registry);

                if let Some(window) = start_window_handle.upgrade() {
                    let _ = window.hide();
                }
                Ok(())
            })();

            if let Err(error) = result {
                eprintln!("failed to start task: {error}");
            }
        });
    }

    start_window.run()?;
    Ok(())
}

fn build_registry_for_task(task_goal: &str) -> Result<(Rc<RefCell<UiRegistry>>, MaterializedAgent)> {
    let mut runtime = RuntimeSupervisor::new();
    let namespace = runtime_namespace();
    let task_id = format!("task.ui.{}", current_timestamp_ms());
    let task_title = summarize_task_title(task_goal);
    let mut workbench = bootstrap_task_workbench(
        &mut runtime,
        TaskRequest::new(task_id, task_title, task_goal.to_owned()).with_namespace(TaskNamespace::new(
            namespace.tenant_id.clone(),
            namespace.user_id.clone(),
        )),
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

    let registry = Rc::new(RefCell::new(UiRegistry {
        runtime,
        orchestrator: AgentOrchestrator::new(),
        session_id: workbench.session.id.clone(),
        task_id: workbench.task.id.clone(),
        task_title: workbench.task.title.clone(),
        task_goal: workbench.task.goal.clone(),
        task_plan: workbench.task_plan.clone(),
        workspace_phase: workbench.phase.clone(),
        namespace,
        agents: workbench.agents,
        windows: Vec::new(),
        next_window_index: 0,
        events_rx,
        events_tx,
    }));

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
        agent.capabilities
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

    {
        let registry = Rc::clone(&registry);
        window.on_new_window(move || {
            if let Err(error) = spawn_seeded_window(Rc::clone(&registry)) {
                eprintln!("failed to open window: {error}");
            }
        });
    }

    {
        let registry = Rc::clone(&registry);
        let window_index = window_index as i32;
        window.on_close_window(move || {
            if let Err(error) = close_window(Rc::clone(&registry), &weak, window_index) {
                eprintln!("failed to close window: {error}");
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
                eprintln!("input error: {error}");
            }
        });
    }

    window.show()?;

    {
        let mut registry_ref = registry.borrow_mut();
        registry_ref.windows.push(WindowController {
            window_index: window_index as i32,
            instance_id: agent.binding.instance_id.clone(),
            instance_name: agent.persona.name.clone(),
            role_name: agent.persona.role.clone(),
            focused_target: None,
            pending_replies: Vec::new(),
            handle: window.clone_strong(),
            transcript_lines: vec![
                "Honeycomb agent window ready.".to_owned(),
                format!("namespace: {}", namespace_label),
                format!("task goal: {}", agent.seed.goal),
                "phase: planning".to_owned(),
                format!("persona: {} ({})", agent.persona.name, agent.persona.role),
                format!("capabilities: {capability_list}"),
                format!(
                    "responder: {}",
                    agent.binding
                        .responder
                        .as_ref()
                        .map(|responder| responder.label())
                        .unwrap_or_else(|| "not bound".to_owned())
                ),
                "Use /msg <name> <text> to talk to another agent.".to_owned(),
                "Use @agent <text> to direct a message without command syntax.".to_owned(),
                "Use /focus <name> to set the current target agent.".to_owned(),
                "Use /responder human|llm to switch how this agent answers.".to_owned(),
                "Use /reply <text> to answer the oldest pending human-routed request.".to_owned(),
                "Use `hc-responder-cli inbox list` or `hc-responder-cli inbox reply-next <text>` in another terminal for remote human replies.".to_owned(),
                "Use /all <text> for broadcast and /channel <name> <text> for channel chat.".to_owned(),
                "Use /run <command> for explicit local shell execution.".to_owned(),
                "Use /channel-create <name>, /join <name>, /leave <name>, /channels.".to_owned(),
                "Use /name <new-name> to rename this window instance.".to_owned(),
                "Use /who to list instances.".to_owned(),
            ],
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

    match line {
        "/help" => {
            append_local_line(
                &registry,
                window_index,
                "Commands: /msg <name> <text>, @name <text>, /focus <name>, /responder human|llm, /pending, /reply <text>, /all <text>, /channel <name> <text>, /run <command>, /plan note <text>, /plan stage <stage> | <title> | <goal>, /plan agent <role> | <reason>, /plan approve, /channel-create <name>, /join <name>, /leave <name>, /channels, /name <new-name>, /who, /help".to_owned(),
            );
        }
        "/plan approve" => {
            approve_plan(&registry, window_index)?;
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

        let result = registry_ref.runtime.dispatch(RuntimeCommand::RenameInstance {
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
        registry_ref.windows[target_index]
            .handle
            .set_window_title(format!("{} | Honeycomb | {}", instance.name, namespace_label).into());
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

fn window_focus_target(
    registry: &Rc<RefCell<UiRegistry>>,
    window_index: i32,
) -> Option<String> {
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
            provider: env::var("HC_LLM_PROVIDER").unwrap_or_else(|_| "openai".to_owned()),
            model: env::var("HC_LLM_MODEL").unwrap_or_else(|_| "gpt-4.1-mini".to_owned()),
            system_prompt: Some(format!(
                "You are {}. Role: {}. Stay concise and collaborative.",
                instance_name, role_name
            )),
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

fn queue_human_reply(
    registry_ref: &mut UiRegistry,
    request: ReplyRequest,
) -> Result<()> {
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
        workspace_root(),
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
        registry_ref.windows[target_index].pending_replies.push(request);
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
        if registry_ref.windows[source_index].pending_replies.is_empty() {
            bail!("no pending human replies");
        }

        let request = registry_ref.windows[source_index].pending_replies.remove(0);
        let source_name = registry_ref.windows[source_index].instance_name.clone();
        let repository = HumanInboxRepository::with_namespace(
            workspace_root(),
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
            .push(format!("[recv {}] {from_name} -> you: {}", message.id, message.body));

        let orchestrator = registry_ref.orchestrator.clone();
        let agents = registry_ref.agents.clone();
        if let Some(agent) = agents.iter().find(|agent| agent.binding.instance_id == to_id) {
            match agent.binding.responder.as_ref() {
                Some(responder) if responder.is_human() => {
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
                }
                Some(ResponderBinding::Llm(_)) => {
                    let reply_backend = default_reply_backend();
                    match orchestrator.generate_and_post_direct_reply(
                        &reply_backend,
                        &mut registry_ref.runtime,
                        &agents,
                        &message.id,
                        &to_id,
                    ) {
                        Ok(reply) => {
                            registry_ref.windows[to_index].transcript_lines.push(format!(
                                "[reply {}] you -> {}: {}",
                                reply.id, from_name, reply.body
                            ));
                            registry_ref.windows[from_index].transcript_lines.push(format!(
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
                Some(other) => {
                    registry_ref.windows[from_index].transcript_lines.push(format!(
                        "[responder] {} is using {}, auto reply not implemented yet",
                        to_name,
                        other.label()
                    ));
                }
                None => {
                    registry_ref.windows[from_index]
                        .transcript_lines
                        .push(format!("[responder] {} has no responder bound", to_name));
                }
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
                window
                    .transcript_lines
                    .push(format!("[broadcast {}] {from_name}: {}", message.id, message.body));
            }
        }

        let orchestrator = registry_ref.orchestrator.clone();
        let agents = registry_ref.agents.clone();
        if let Some(grant) = orchestrator.run_nomination_cycle(
            &mut registry_ref.runtime,
            &agents,
            &message,
            current_timestamp_ms(),
        )? {
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
            match winning_agent.and_then(|agent| agent.binding.responder.as_ref()) {
                Some(responder) if responder.is_human() => {
                    let request = orchestrator.build_reply_request_for_grant(
                        &registry_ref.runtime,
                        &agents,
                        &grant,
                    )?;
                    queue_human_reply(&mut registry_ref, request)?;
                    for window in &mut registry_ref.windows {
                        window.transcript_lines.push(format!(
                            "[pending] {} will reply manually",
                            speaker_name
                        ));
                    }
                }
                Some(ResponderBinding::Llm(_)) => {
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
                                } else if matches!(&reply.route, MessageRoute::Direct { to } if to == &window.instance_id) {
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
                Some(other) => {
                    for window in &mut registry_ref.windows {
                        window.transcript_lines.push(format!(
                            "[responder] {} is using {}, auto reply not implemented yet",
                            speaker_name,
                            other.label()
                        ));
                    }
                }
                None => {}
            }
        }
    }

    sync_windows(registry);
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
        let result = registry_ref.runtime.dispatch(RuntimeCommand::CreateChannel {
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
        let result = registry_ref.runtime.dispatch(RuntimeCommand::LeaveChannel {
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
        if let Some(grant) = orchestrator.run_nomination_cycle(
            &mut registry_ref.runtime,
            &agents,
            &message,
            current_timestamp_ms(),
        )? {
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
            match winning_agent.and_then(|agent| agent.binding.responder.as_ref()) {
                Some(responder) if responder.is_human() => {
                    let request = orchestrator.build_reply_request_for_grant(
                        &registry_ref.runtime,
                        &agents,
                        &grant,
                    )?;
                    queue_human_reply(&mut registry_ref, request)?;
                    for window in &mut registry_ref.windows {
                        window.transcript_lines.push(format!(
                            "[pending] {} will reply manually",
                            speaker_name
                        ));
                    }
                }
                Some(ResponderBinding::Llm(_)) => {
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
                                } else if matches!(&reply.route, MessageRoute::Direct { to } if to == &window.instance_id) {
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
                Some(other) => {
                    for window in &mut registry_ref.windows {
                        window.transcript_lines.push(format!(
                            "[responder] {} is using {}, auto reply not implemented yet",
                            speaker_name,
                            other.label()
                        ));
                    }
                }
                None => {}
            }
        }
    }

    sync_windows(registry);
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
        let spawn_result = Command::new("powershell")
            .args(["-NoProfile", "-Command", &line])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn();

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
                UiEvent::CommandExit { window_index, status } => {
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
            workspace_root(),
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
            workspace_root(),
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
        let titles = registry_ref
            .windows
            .iter()
            .map(|window| SharedString::from(format!("{} [{}]", window.instance_name, window.role_name)))
            .collect::<Vec<_>>();

        let snapshots = registry_ref
            .windows
            .iter()
            .map(|window| {
                let transcript_text = render_window_transcript(
                    &workspace_view,
                    &window.instance_name,
                    &window.role_name,
                    window.focused_target.as_deref(),
                    &window.transcript_lines,
                );
                let agent_board_text = render_agent_board(&workspace_view);
                let inspector_text = render_inspector(
                    &workspace_view,
                    &window.instance_id,
                    &window.instance_name,
                    window.focused_target.as_deref(),
                );
                (
                    window.handle.clone_strong(),
                    render_prompt_label(&workspace_view, window),
                    agent_board_text,
                    inspector_text,
                    transcript_text,
                )
            })
            .collect::<Vec<_>>();

        (titles, snapshots)
    };

    let model = ModelRc::new(VecModel::from(titles));
    for (handle, prompt_label, agent_board_text, inspector_text, transcript_text) in snapshots {
        handle.set_open_window_titles(model.clone());
        handle.set_prompt_label(prompt_label.into());
        handle.set_agent_board_text(agent_board_text.into());
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

fn render_window_transcript(
    workspace: &hc_agent::WorkspaceViewModel,
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
            "Plan: {} | Work items: {} | Agent proposals: {}",
            workspace.plan_status, workspace.work_item_count, workspace.proposed_agent_count
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

fn render_inspector(
    workspace: &hc_agent::WorkspaceViewModel,
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
        lines.extend(workspace.recent_activity.iter().take(3).map(render_activity_line));
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

    lines.join("\n")
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
        "{} | {} | phase {} | responder {} | pending {} | target {}",
        workspace.task_title,
        window.instance_name,
        workspace.phase,
        responder,
        pending,
        window.focused_target.as_deref().unwrap_or("workspace")
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
            provider: env::var("HC_LLM_PROVIDER").unwrap_or_else(|_| "openai".to_owned()),
            model: env::var("HC_LLM_MODEL").unwrap_or_else(|_| "gpt-4.1-mini".to_owned()),
            system_prompt: Some(format!(
                "You are {}. Role: {}. Stay concise and collaborative.",
                agent_name, role_name
            )),
        })
    } else {
        ResponderBinding::Human(HumanResponderConfig::new(
            Some(env::var("HC_USER_ID").unwrap_or_else(|_| "default".to_owned())),
            None,
        ))
    }
}

fn has_configured_llm_provider() -> bool {
    env::var("HC_LLM_API_KEY").is_ok() || env::var("OPENAI_API_KEY").is_ok()
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
            (proposal.id.clone(), proposal.role.clone(), proposal.reason.clone())
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
    drop(registry_ref);
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

fn runtime_namespace() -> RuntimeNamespace {
    let tenant_id = env::var("HC_TENANT_ID").unwrap_or_else(|_| "local".to_owned());
    let user_id = env::var("HC_USER_ID").unwrap_or_else(|_| "default".to_owned());
    RuntimeNamespace::new(tenant_id, user_id)
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
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

fn workspace_root() -> std::path::PathBuf {
    std::path::PathBuf::from("workspace")
}

fn default_llm_registry() -> ProviderRegistry {
    let mut registry = ProviderRegistry::new();
    let provider_id = env::var("HC_LLM_PROVIDER").unwrap_or_else(|_| "openai".to_owned());
    let api_key = env::var("HC_LLM_API_KEY")
        .or_else(|_| env::var("OPENAI_API_KEY"))
        .ok();
    let base_url = env::var("HC_LLM_BASE_URL")
        .or_else(|_| env::var("OPENAI_BASE_URL"))
        .unwrap_or_else(|_| match provider_id.as_str() {
            "minimax" => "https://api.minimaxi.com/v1".to_owned(),
            _ => "https://api.openai.com/v1".to_owned(),
        });

    if let Some(api_key) = api_key {
        if let Ok(provider) = OpenAiCompatibleProvider::new(
            provider_id.clone(),
            format!("{provider_id} compatible"),
            base_url,
            api_key,
        ) {
            registry.register(provider);
        }
    }

    registry
}

fn default_reply_backend() -> RegistryReplyBackend {
    RegistryReplyBackend {
        registry: default_llm_registry(),
    }
}
