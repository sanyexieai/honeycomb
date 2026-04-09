use std::{
    cell::RefCell,
    io::{BufRead, BufReader},
    process::{Command, Stdio},
    rc::Rc,
    sync::mpsc::{self, Receiver, Sender},
    thread,
    time::Duration,
};

use anyhow::{Context, Result, bail};
use hc_core::{MessageKind, MessageRoute, RuntimeCommand, RuntimeCommandResult, RuntimeSupervisor};
use slint::{ComponentHandle, ModelRc, Timer, TimerMode, VecModel, Weak};

slint::slint! {
    import { Button, HorizontalBox, LineEdit, TextEdit, VerticalBox } from "std-widgets.slint";

    export component MultiWindowShell inherits Window {
        in property <string> window-title;
        in property <string> role-name;
        in property <int> window-index;
        in property <[string]> open-window-titles;
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

            Rectangle {
                border-radius: 12px;
                background: #faf7f0;
                border-width: 1px;
                border-color: #d7d1c6;
                vertical-stretch: 1;

                TextEdit {
                    x: 0;
                    y: 0;
                    width: parent.width;
                    height: parent.height;
                    read-only: true;
                    text: transcript-text;
                }
            }

            HorizontalBox {
                spacing: 8px;

                input := LineEdit {
                    placeholder-text: "Type /msg <name> <text> or a local command";
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
    handle: MultiWindowShell,
    transcript_lines: Vec<String>,
}

struct UiRegistry {
    runtime: RuntimeSupervisor,
    session_id: String,
    windows: Vec<WindowController>,
    next_window_index: usize,
    events_rx: Receiver<UiEvent>,
    events_tx: Sender<UiEvent>,
}

enum UiEvent {
    CommandOutput { window_index: i32, line: String },
    CommandExit { window_index: i32, status: String },
}

pub fn run() -> Result<()> {
    let mut runtime = RuntimeSupervisor::new();
    let session = runtime.create_session("ui-shell");
    let (events_tx, events_rx) = mpsc::channel();
    let registry = Rc::new(RefCell::new(UiRegistry {
        runtime,
        session_id: session.id,
        windows: Vec::new(),
        next_window_index: 0,
        events_rx,
        events_tx,
    }));

    let timer = Timer::default();
    {
        let registry = Rc::clone(&registry);
        timer.start(TimerMode::Repeated, Duration::from_millis(100), move || {
            pump_ui_events(&registry);
        });
    }

    let main_window = spawn_window(Rc::clone(&registry), "main-shell".to_owned())?;
    main_window.run()?;
    Ok(())
}

fn spawn_window(registry: Rc<RefCell<UiRegistry>>, role_name: String) -> Result<MultiWindowShell> {
    let window = MultiWindowShell::new()?;
    let weak = window.as_weak();

    let (window_index, instance_id, instance_name) = {
        let mut registry_ref = registry.borrow_mut();
        let window_index = registry_ref.next_window_index;
        registry_ref.next_window_index += 1;
        let session_id = registry_ref.session_id.clone();

        let result = registry_ref.runtime.dispatch(RuntimeCommand::CreateInstance {
            session_id,
            name: role_name,
            parent_instance_id: None,
        })?;
        let RuntimeCommandResult::Instance(instance) = result else {
            bail!("unexpected runtime result while creating instance");
        };

        (window_index, instance.id, instance.name)
    };

    window.set_window_title(format!("{} | Honeycomb", instance_name).into());
    window.set_role_name(instance_name.clone().into());
    window.set_window_index(window_index as i32);
    window.set_prompt_label(
        format!("{} | /msg <name> <text> or a local command", instance_name).into(),
    );

    {
        let registry = Rc::clone(&registry);
        window.on_new_window(move || {
            let role_name = format!("worker-{}", registry.borrow().next_window_index + 1);
            if let Err(error) = spawn_window(Rc::clone(&registry), role_name) {
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
            if let Err(error) = handle_window_input(Rc::clone(&registry), window_index, text.to_string())
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
            instance_id,
            instance_name,
            handle: window.clone_strong(),
        transcript_lines: vec![
                "Honeycomb instance window ready.".to_owned(),
                "Use /msg <name> <text> to talk to another window.".to_owned(),
                "Use /all <text> for broadcast and /channel <name> <text> for channel chat.".to_owned(),
                "Use /channel-create <name>, /join <name>, /leave <name>, /channels.".to_owned(),
                "Use /name <new-name> to rename this window instance.".to_owned(),
                "Use /who to list instances.".to_owned(),
            ],
        });
    }
    sync_windows(&registry);

    Ok(window)
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

    match line {
        "/help" => {
            append_local_line(
                &registry,
                window_index,
                "Commands: /msg <name> <text>, /all <text>, /channel <name> <text>, /channel-create <name>, /join <name>, /leave <name>, /channels, /name <new-name>, /who, /help".to_owned(),
            );
        }
        "/who" => {
            let names = {
                let registry_ref = registry.borrow();
                registry_ref
                    .windows
                    .iter()
                    .map(|window| window.instance_name.clone())
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
            append_local_line(&registry, window_index, format!("$ {line}"));
            run_local_command_async(Rc::clone(&registry), window_index, line.to_owned());
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
        registry_ref.windows[target_index]
            .handle
            .set_window_title(format!("{} | Honeycomb", instance.name).into());
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

fn send_window_message(
    registry: &Rc<RefCell<UiRegistry>>,
    from_window_index: i32,
    target_name: &str,
    body: &str,
) -> Result<()> {
    let (session_id, from_id, from_name, to_id, to_name, message_body) = {
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

        (session_id, from_id, from_name, to_id, to_name, message.body)
    };

    let _ = (session_id, from_id, from_name, to_id, to_name, message_body);
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
                let reader = BufReader::new(stdout);
                for line in reader.lines() {
                    match line {
                        Ok(line) => {
                            let _ = sender.send(UiEvent::CommandOutput { window_index, line });
                        }
                        Err(error) => {
                            let _ = sender.send(UiEvent::CommandOutput {
                                window_index,
                                line: format!("stdout read error: {error}"),
                            });
                            break;
                        }
                    }
                }
            });
        }

        if let Some(stderr) = stderr {
            let sender = sender.clone();
            let _ = thread::spawn(move || {
                let reader = BufReader::new(stderr);
                for line in reader.lines() {
                    match line {
                        Ok(line) => {
                            let _ = sender.send(UiEvent::CommandOutput {
                                window_index,
                                line: format!("stderr: {line}"),
                            });
                        }
                        Err(error) => {
                            let _ = sender.send(UiEvent::CommandOutput {
                                window_index,
                                line: format!("stderr read error: {error}"),
                            });
                            break;
                        }
                    }
                }
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

fn pump_ui_events(registry: &Rc<RefCell<UiRegistry>>) {
    let mut changed = false;

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

fn sync_windows(registry: &Rc<RefCell<UiRegistry>>) {
    let (titles, snapshots) = {
        let registry_ref = registry.borrow();
        let titles = registry_ref
            .windows
            .iter()
            .map(|window| window.handle.get_window_title())
            .collect::<Vec<_>>();

        let snapshots = registry_ref
            .windows
            .iter()
            .map(|window| {
                (
                    window.handle.clone_strong(),
                    format!("{} | instance {}", window.instance_name, window.instance_id),
                    window.transcript_lines.join("\n"),
                )
            })
            .collect::<Vec<_>>();

        (titles, snapshots)
    };

    let model = ModelRc::new(VecModel::from(titles));
    for (handle, prompt_label, transcript_text) in snapshots {
        handle.set_open_window_titles(model.clone());
        handle.set_prompt_label(prompt_label.into());
        handle.set_transcript_text(transcript_text.into());
    }
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

fn parse_name_command(line: &str) -> Option<String> {
    let rest = line.strip_prefix("/name ")?;
    let name = rest.trim();
    if name.is_empty() {
        return None;
    }
    Some(name.to_owned())
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
