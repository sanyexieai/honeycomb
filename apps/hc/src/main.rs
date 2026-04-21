use std::{
    env, fs,
    io::{self, BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use hc_core::{
    JobState, MessageKind, MessageRoute, ParticipationClaim, ProcessWorker, RunMode, RunRequest,
    RuntimeCommand, RuntimeCommandResult, RuntimeNamespace, RuntimeState, RuntimeSupervisor,
    WorkerReport,
};

fn main() -> Result<()> {
    let args: Vec<String> = env::args().skip(1).collect();

    if args.is_empty() {
        return run_control_repl();
    }

    match args[0].as_str() {
        "demo" => run_demo(),
        "reset" => reset_state(),
        "session" => handle_session(&args[1..]),
        "instance" => handle_instance(&args[1..]),
        "channel" => handle_channel(&args[1..]),
        "send" => handle_send(&args[1..]),
        "claim" => handle_claim(&args[1..]),
        "term" => handle_term(&args[1..]),
        "chat" => handle_chat(&args[1..]),
        "inbox" => handle_inbox(&args[1..]),
        "events" => handle_events(&args[1..]),
        "watch" => handle_watch(&args[1..]),
        "help" | "--help" | "-h" => {
            print_help();
            Ok(())
        }
        other => {
            bail!("unknown command: {other}")
        }
    }
}

fn run_control_repl() -> Result<()> {
    println!("hc control terminal. Type /help for commands, /quit to exit.");

    loop {
        print!("hc> ");
        io::stdout().flush().context("failed to flush stdout")?;

        let mut line = String::new();
        let read = io::stdin()
            .read_line(&mut line)
            .context("failed to read stdin")?;
        if read == 0 {
            break;
        }

        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        if line == "/quit" || line == "/exit" {
            break;
        }

        if line == "/help" {
            print_control_help();
            continue;
        }

        let tokens = split_command(line);
        if tokens.is_empty() {
            continue;
        }

        match tokens[0].as_str() {
            "/session" => handle_session_tokens(&tokens[1..])?,
            "/instance" => handle_instance_tokens(&tokens[1..])?,
            "/channel" => handle_channel_tokens(&tokens[1..])?,
            "/claim" => handle_claim_tokens(&tokens[1..])?,
            "/term" => handle_term_tokens(&tokens[1..])?,
            "/reset" => reset_state()?,
            "/help" => print_control_help(),
            _ => run_local_command(line)?,
        }
    }

    Ok(())
}

fn handle_session(args: &[String]) -> Result<()> {
    match args {
        [action] if action == "list" => {
            let runtime = load_runtime()?;
            for session in &runtime.state().sessions {
                println!("{} {}", session.id, session.name);
            }
            Ok(())
        }
        [action, name] if action == "create" => {
            let namespace = runtime_namespace();
            let session = with_locked_runtime_mut(|runtime| {
                let result = runtime.dispatch(RuntimeCommand::CreateSession {
                    name: name.clone(),
                    namespace: Some(namespace.clone()),
                })?;
                let RuntimeCommandResult::Session(session) = result else {
                    bail!("unexpected runtime result");
                };
                Ok(session)
            })?;
            println!("created session {} {}", session.id, session.name);
            Ok(())
        }
        _ => {
            bail!("usage: hc session create <name> | hc session list")
        }
    }
}

fn handle_session_tokens(tokens: &[String]) -> Result<()> {
    match tokens {
        [action] if action == "list" => handle_session(&["list".to_owned()]),
        [action, name] if action == "create" => {
            handle_session(&["create".to_owned(), name.clone()])
        }
        _ => {
            println!("usage: /session create <name> | /session list");
            Ok(())
        }
    }
}

fn handle_instance(args: &[String]) -> Result<()> {
    match args {
        [action, session_selector, name] if action == "create" => {
            let instance = with_locked_runtime_mut(|runtime| {
                let session_id = resolve_session_selector(runtime, session_selector)?;
                let result = runtime.dispatch(RuntimeCommand::CreateInstance {
                    session_id,
                    name: name.clone(),
                    parent_instance_id: None,
                })?;
                let RuntimeCommandResult::Instance(instance) = result else {
                    bail!("unexpected runtime result");
                };
                Ok(instance)
            })?;
            println!("created instance {} {}", instance.id, instance.name);
            Ok(())
        }
        [action, session_selector] if action == "list" => {
            let runtime = load_runtime()?;
            let session_id = resolve_session_selector(&runtime, session_selector)?;
            for instance in runtime
                .state()
                .instances
                .iter()
                .filter(|instance| instance.session_id == session_id)
            {
                println!("{} {}", instance.id, instance.name);
            }
            Ok(())
        }
        _ => {
            bail!("usage: hc instance create <session> <name> | hc instance list <session>")
        }
    }
}

fn handle_instance_tokens(tokens: &[String]) -> Result<()> {
    match tokens {
        [action, session, name] if action == "create" => {
            handle_instance(&["create".to_owned(), session.clone(), name.clone()])
        }
        [action, session] if action == "list" => {
            handle_instance(&["list".to_owned(), session.clone()])
        }
        _ => {
            println!("usage: /instance create <session> <name> | /instance list <session>");
            Ok(())
        }
    }
}

fn handle_channel(args: &[String]) -> Result<()> {
    match args {
        [action, session_selector, name] if action == "create" => {
            let channel = with_locked_runtime_mut(|runtime| {
                let session_id = resolve_session_selector(runtime, session_selector)?;
                let result = runtime.dispatch(RuntimeCommand::CreateChannel {
                    session_id,
                    name: name.clone(),
                })?;
                let RuntimeCommandResult::Channel(channel) = result else {
                    bail!("unexpected runtime result");
                };
                Ok(channel)
            })?;
            println!("created channel {} {}", channel.id, channel.name);
            Ok(())
        }
        [action, session_selector] if action == "list" => {
            let runtime = load_runtime()?;
            let session_id = resolve_session_selector(&runtime, session_selector)?;
            for channel in runtime
                .state()
                .channels
                .iter()
                .filter(|channel| channel.session_id == session_id)
            {
                println!("{} {}", channel.id, channel.name);
            }
            Ok(())
        }
        [
            action,
            session_selector,
            instance_selector,
            channel_selector,
        ] if action == "join" => {
            let instance = with_locked_runtime_mut(|runtime| {
                let session_id = resolve_session_selector(runtime, session_selector)?;
                let instance_id =
                    resolve_instance_selector(runtime, &session_id, instance_selector)?;
                let channel_id = resolve_channel_selector(runtime, &session_id, channel_selector)?;
                let result = runtime.dispatch(RuntimeCommand::JoinChannel {
                    instance_id,
                    channel_id,
                })?;
                let RuntimeCommandResult::Instance(instance) = result else {
                    bail!("unexpected runtime result");
                };
                Ok(instance)
            })?;
            println!(
                "joined {} to channel(s): {}",
                instance.name,
                instance.channel_ids.join(", ")
            );
            Ok(())
        }
        [
            action,
            session_selector,
            instance_selector,
            channel_selector,
        ] if action == "leave" => {
            let instance = with_locked_runtime_mut(|runtime| {
                let session_id = resolve_session_selector(runtime, session_selector)?;
                let instance_id =
                    resolve_instance_selector(runtime, &session_id, instance_selector)?;
                let channel_id = resolve_channel_selector(runtime, &session_id, channel_selector)?;
                let result = runtime.dispatch(RuntimeCommand::LeaveChannel {
                    instance_id,
                    channel_id,
                })?;
                let RuntimeCommandResult::Instance(instance) = result else {
                    bail!("unexpected runtime result");
                };
                Ok(instance)
            })?;
            println!("left channel(s): {}", instance.channel_ids.join(", "));
            Ok(())
        }
        [
            action,
            session_selector,
            from_selector,
            channel_selector,
            message @ ..,
        ] if action == "send" && !message.is_empty() => {
            let body = message.join(" ");
            let (from_label, route_label, body) = with_locked_runtime_mut(|runtime| {
                let session_id = resolve_session_selector(runtime, session_selector)?;
                let from = resolve_instance_selector(runtime, &session_id, from_selector)?;
                let channel_id = resolve_channel_selector(runtime, &session_id, channel_selector)?;
                let result = runtime.dispatch(RuntimeCommand::PostMessage {
                    session_id,
                    from: from.clone(),
                    route: MessageRoute::Channel { channel_id },
                    kind: MessageKind::Chat,
                    body: body.clone(),
                    reply_to: None,
                })?;
                let RuntimeCommandResult::Message(message) = result else {
                    bail!("unexpected runtime result");
                };
                let from_label = display_instance(runtime, &message.session_id, &message.from);
                let route_label =
                    display_message_route(runtime, &message.session_id, &message.route);
                Ok((from_label, route_label, message.body))
            })?;
            println!("[{from_label} -> {route_label}] {body}");
            Ok(())
        }
        _ => bail!(
            "usage: hc channel create <session> <name> | hc channel list <session> | hc channel join <session> <instance> <channel> | hc channel leave <session> <instance> <channel> | hc channel send <session> <from_instance> <channel> <message...>"
        ),
    }
}

fn handle_channel_tokens(tokens: &[String]) -> Result<()> {
    match tokens {
        [action, session, name] if action == "create" => {
            handle_channel(&["create".to_owned(), session.clone(), name.clone()])
        }
        [action, session] if action == "list" => {
            handle_channel(&["list".to_owned(), session.clone()])
        }
        [action, session, instance, channel] if action == "join" => handle_channel(&[
            "join".to_owned(),
            session.clone(),
            instance.clone(),
            channel.clone(),
        ]),
        [action, session, instance, channel] if action == "leave" => handle_channel(&[
            "leave".to_owned(),
            session.clone(),
            instance.clone(),
            channel.clone(),
        ]),
        _ => {
            println!(
                "usage: /channel create <session> <name> | /channel list <session> | /channel join <session> <instance> <channel> | /channel leave <session> <instance> <channel>"
            );
            Ok(())
        }
    }
}

fn handle_send(args: &[String]) -> Result<()> {
    if args.len() >= 4 && args[0] == "--all" {
        let session_selector = &args[1];
        let from_selector = &args[2];
        let body = args[3..].join(" ");
        let (from_label, route_label, body) = with_locked_runtime_mut(|runtime| {
            let session_id = resolve_session_selector(runtime, session_selector)?;
            let from = resolve_instance_selector(runtime, &session_id, from_selector)?;
            let result = runtime.dispatch(RuntimeCommand::PostMessage {
                session_id,
                from: from.clone(),
                route: MessageRoute::Broadcast,
                kind: MessageKind::Chat,
                body: body.clone(),
                reply_to: None,
            })?;
            let RuntimeCommandResult::Message(message) = result else {
                bail!("unexpected runtime result");
            };
            let from_label = display_instance(runtime, &message.session_id, &message.from);
            let route_label = display_message_route(runtime, &message.session_id, &message.route);
            Ok((from_label, route_label, message.body))
        })?;
        println!("[{from_label} -> {route_label}] {body}");
        return Ok(());
    }

    if args.len() < 4 {
        bail!(
            "usage: hc send <session> <from_instance> <to_instance> <message...> | hc send --all <session> <from_instance> <message...>"
        );
    }

    let session_selector = &args[0];
    let from_selector = &args[1];
    let to_selector = &args[2];
    let body = args[3..].join(" ");

    let (from_label, to_label, body) = with_locked_runtime_mut(|runtime| {
        let session_id = resolve_session_selector(runtime, session_selector)?;
        let from = resolve_instance_selector(runtime, &session_id, from_selector)?;
        let to = resolve_instance_selector(runtime, &session_id, to_selector)?;
        let result = runtime.dispatch(RuntimeCommand::PostMessage {
            session_id,
            from,
            route: MessageRoute::Direct { to },
            kind: MessageKind::Chat,
            body: body.clone(),
            reply_to: None,
        })?;
        let RuntimeCommandResult::Message(message) = result else {
            bail!("unexpected runtime result");
        };
        let from_label = display_instance(runtime, &message.session_id, &message.from);
        let to_label = display_message_route(runtime, &message.session_id, &message.route);
        Ok((from_label, to_label, message.body))
    })?;
    println!("[{from_label} -> {to_label}] {body}");
    Ok(())
}

fn handle_claim(args: &[String]) -> Result<()> {
    match args {
        [
            action,
            session_selector,
            instance_selector,
            message_id,
            score,
        ] if action == "submit" => {
            handle_claim_submit(session_selector, instance_selector, message_id, score, None)
        }
        [
            action,
            session_selector,
            instance_selector,
            message_id,
            score,
            reason @ ..,
        ] if action == "submit" => handle_claim_submit(
            session_selector,
            instance_selector,
            message_id,
            score,
            Some(reason.join(" ")),
        ),
        [action, message_id] if action == "list" => {
            let runtime = load_runtime()?;
            for claim in runtime.claims_for_message(message_id)? {
                let session_id = runtime
                    .state()
                    .messages
                    .iter()
                    .find(|message| message.id == *message_id)
                    .map(|message| message.session_id.clone())
                    .context("message should exist")?;
                let label = display_instance(&runtime, &session_id, &claim.instance_id);
                println!(
                    "{} score={:.2} round={} reason={}",
                    label,
                    claim.score,
                    claim.round,
                    claim.reason.as_deref().unwrap_or("-")
                );
            }
            Ok(())
        }
        [action, message_id, round] if action == "resolve" => {
            let round = round
                .parse::<u32>()
                .with_context(|| format!("invalid round: {round}"))?;
            let result = with_locked_runtime_mut(|runtime| {
                let result = runtime.dispatch(RuntimeCommand::ResolveSpeakingGrant {
                    message_id: message_id.clone(),
                    round,
                })?;
                let RuntimeCommandResult::SpeakingGrant(grant) = result else {
                    bail!("unexpected runtime result");
                };
                Ok((runtime.state().clone(), grant))
            })?;
            let (state, grant) = result;
            match grant {
                Some(grant) => {
                    let runtime = RuntimeSupervisor::from_state(state);
                    let session_id = runtime
                        .state()
                        .messages
                        .iter()
                        .find(|message| message.id == grant.message_id)
                        .map(|message| message.session_id.clone())
                        .context("message should exist")?;
                    let label = display_instance(&runtime, &session_id, &grant.instance_id);
                    println!(
                        "granted {} for {} in round {} score={:.2}",
                        label, grant.message_id, grant.round, grant.score
                    );
                }
                None => {
                    println!("no speaking grant for {message_id} in round {round}");
                }
            }
            Ok(())
        }
        _ => bail!(
            "usage: hc claim submit <session> <instance> <message_id> <score> [reason...] | hc claim list <message_id> | hc claim resolve <message_id> <round>"
        ),
    }
}

fn handle_claim_submit(
    session_selector: &str,
    instance_selector: &str,
    message_id: &str,
    score: &str,
    reason: Option<String>,
) -> Result<()> {
    let score = score
        .parse::<f32>()
        .with_context(|| format!("invalid score: {score}"))?;
    let claim = with_locked_runtime_mut(|runtime| {
        let session_id = resolve_session_selector(runtime, session_selector)?;
        let instance_id = resolve_instance_selector(runtime, &session_id, instance_selector)?;
        let round = runtime
            .nomination_policy()
            .rounds
            .first()
            .map(|round| round.round)
            .unwrap_or(1);
        let timestamp_ms = current_timestamp_ms();
        let claim = match reason.clone() {
            Some(reason) => ParticipationClaim::new(
                message_id.to_owned(),
                instance_id,
                score,
                round,
                timestamp_ms,
            )
            .with_reason(reason),
            None => ParticipationClaim::new(
                message_id.to_owned(),
                instance_id,
                score,
                round,
                timestamp_ms,
            ),
        };
        let result = runtime.dispatch(RuntimeCommand::SubmitParticipationClaim {
            claim: claim.clone(),
        })?;
        let RuntimeCommandResult::Claim(claim) = result else {
            bail!("unexpected runtime result");
        };
        Ok((runtime.state().clone(), claim))
    })?;
    let (state, claim) = claim;
    let runtime = RuntimeSupervisor::from_state(state);
    let session_id = runtime
        .state()
        .messages
        .iter()
        .find(|message| message.id == claim.message_id)
        .map(|message| message.session_id.clone())
        .context("message should exist")?;
    let label = display_instance(&runtime, &session_id, &claim.instance_id);
    println!(
        "claim submitted {} message={} score={:.2} round={}",
        label, claim.message_id, claim.score, claim.round
    );
    Ok(())
}

fn handle_claim_tokens(tokens: &[String]) -> Result<()> {
    match tokens {
        [action, session, instance, message_id, score] if action == "submit" => handle_claim(&[
            "submit".to_owned(),
            session.clone(),
            instance.clone(),
            message_id.clone(),
            score.clone(),
        ]),
        [action, session, instance, message_id, score, reason @ ..] if action == "submit" => {
            let mut args = vec![
                "submit".to_owned(),
                session.clone(),
                instance.clone(),
                message_id.clone(),
                score.clone(),
            ];
            args.extend(reason.iter().cloned());
            handle_claim(&args)
        }
        [action, message_id] if action == "list" => {
            handle_claim(&["list".to_owned(), message_id.clone()])
        }
        [action, message_id, round] if action == "resolve" => {
            handle_claim(&["resolve".to_owned(), message_id.clone(), round.clone()])
        }
        _ => {
            println!(
                "usage: /claim submit <session> <instance> <message_id> <score> [reason...] | /claim list <message_id> | /claim resolve <message_id> <round>"
            );
            Ok(())
        }
    }
}

fn handle_chat(args: &[String]) -> Result<()> {
    match args {
        [session_selector, from_selector, to_selector] => {
            let runtime = load_runtime()?;
            let session_id = resolve_session_selector(&runtime, session_selector)?;
            let from = resolve_instance_selector(&runtime, &session_id, from_selector)?;
            let to = resolve_instance_selector(&runtime, &session_id, to_selector)?;
            let seen = runtime.mailbox_for_instance(&session_id, &from)?.len();
            let stop = Arc::new(AtomicBool::new(false));
            let watch_stop = Arc::clone(&stop);
            let watch_session = session_selector.to_owned();
            let watch_from = from.clone();

            println!(
                "chatting from {from_selector} to {to_selector} in {session_selector}. Type /quit to exit."
            );
            println!("new incoming messages for {from_selector} will be shown as [recv ...].");
            let watcher = thread::spawn(move || {
                let mut seen = seen;
                while !watch_stop.load(Ordering::Relaxed) {
                    if let Err(error) =
                        print_new_inbox_messages(&watch_session, &watch_from, &mut seen, None)
                    {
                        eprintln!("watch error: {error}");
                    }
                    thread::sleep(Duration::from_millis(600));
                }
            });

            loop {
                print!("> ");
                io::stdout().flush().context("failed to flush stdout")?;

                let mut line = String::new();
                let read = io::stdin()
                    .read_line(&mut line)
                    .context("failed to read stdin")?;
                if read == 0 {
                    break;
                }

                let body = line.trim();
                if body.is_empty() {
                    continue;
                }
                if body == "/quit" || body == "/exit" {
                    break;
                }

                let (to_label, body) = with_locked_runtime_mut(|runtime| {
                    let result = runtime.dispatch(RuntimeCommand::PostMessage {
                        session_id: session_id.clone(),
                        from: from.clone(),
                        route: MessageRoute::Direct { to: to.clone() },
                        kind: MessageKind::Chat,
                        body: body.to_owned(),
                        reply_to: None,
                    })?;
                    let RuntimeCommandResult::Message(message) = result else {
                        bail!("unexpected runtime result");
                    };
                    let to_label =
                        display_message_route(runtime, &message.session_id, &message.route);
                    Ok((to_label, message.body))
                })?;
                println!("[you -> {to_label}] {body}");
            }

            stop.store(true, Ordering::Relaxed);
            let _ = watcher.join();

            Ok(())
        }
        _ => bail!("usage: hc chat <session> <from_instance> <to_instance>"),
    }
}

fn handle_term(args: &[String]) -> Result<()> {
    match args {
        [session_selector, instance_selector] => run_term(session_selector, instance_selector),
        _ => bail!("usage: hc term <session> <instance>"),
    }
}

fn handle_term_tokens(tokens: &[String]) -> Result<()> {
    match tokens {
        [session, instance] => run_term(session, instance),
        _ => {
            println!("usage: /term <session> <instance>");
            Ok(())
        }
    }
}

fn handle_inbox(args: &[String]) -> Result<()> {
    match args {
        [session_selector, instance_selector] => {
            print_inbox(session_selector, instance_selector, None)
        }
        [session_selector, instance_selector, route_filter] => print_inbox(
            session_selector,
            instance_selector,
            Some(route_filter.as_str()),
        ),
        _ => bail!("usage: hc inbox <session> <instance> [route]"),
    }
}

fn print_inbox(
    session_selector: &str,
    instance_selector: &str,
    route_filter: Option<&str>,
) -> Result<()> {
    let runtime = load_runtime()?;
    let session_id = resolve_session_selector(&runtime, session_selector)?;
    let instance_id = resolve_instance_selector(&runtime, &session_id, instance_selector)?;
    let messages = runtime.mailbox_for_instance(&session_id, &instance_id)?;
    for message in messages {
        if !message_matches_filter(&runtime, &session_id, message, route_filter)? {
            continue;
        }
        let from_label = display_instance(&runtime, &session_id, &message.from);
        println!(
            "[{}] {} -> {}: {}",
            message.id,
            from_label,
            display_message_route(&runtime, &session_id, &message.route),
            message.body
        );
    }
    Ok(())
}

fn handle_events(args: &[String]) -> Result<()> {
    match args {
        [session_selector] => {
            let runtime = load_runtime()?;
            let session_id = resolve_session_selector(&runtime, session_selector)?;
            for event in runtime
                .state()
                .events
                .iter()
                .filter(|event| event.session_id == session_id)
            {
                println!(
                    "[{}] {:?} source={} target={} payload={}",
                    event.id,
                    event.kind,
                    event.source,
                    event.target.as_deref().unwrap_or("-"),
                    event.payload.trim_end()
                );
            }
            Ok(())
        }
        [session_selector, instance_selector] => {
            let runtime = load_runtime()?;
            let session_id = resolve_session_selector(&runtime, session_selector)?;
            let instance_id = resolve_instance_selector(&runtime, &session_id, instance_selector)?;
            let events = runtime.events_for_instance(&session_id, &instance_id)?;
            for event in events {
                println!(
                    "[{}] {:?} source={} target={} payload={}",
                    event.id,
                    event.kind,
                    event.source,
                    event.target.as_deref().unwrap_or("-"),
                    event.payload.trim_end()
                );
            }
            Ok(())
        }
        _ => bail!("usage: hc events <session> [instance]"),
    }
}

fn handle_watch(args: &[String]) -> Result<()> {
    match args {
        [target, session_selector, instance_selector] if target == "inbox" => {
            watch_inbox(session_selector, instance_selector)
        }
        [target, session_selector] if target == "events" => watch_events(session_selector, None),
        [target, session_selector, instance_selector] if target == "events" => {
            watch_events(session_selector, Some(instance_selector.as_str()))
        }
        _ => bail!(
            "usage: hc watch inbox <session> <instance> | hc watch events <session> [instance]"
        ),
    }
}

fn run_term(session_selector: &str, instance_selector: &str) -> Result<()> {
    let runtime = load_runtime()?;
    let session_id = resolve_session_selector(&runtime, session_selector)?;
    let instance_id = resolve_instance_selector(&runtime, &session_id, instance_selector)?;
    let instance_label = display_instance(&runtime, &session_id, &instance_id);
    let seen = runtime
        .mailbox_for_instance(&session_id, &instance_id)?
        .len();
    let stop = Arc::new(AtomicBool::new(false));
    let watch_stop = Arc::clone(&stop);
    let watch_session = session_selector.to_owned();
    let watch_instance = instance_id.clone();

    println!("term {instance_label} in {session_selector}. Type /help for commands.");
    let prompt_label = instance_label.clone();
    let watcher = thread::spawn(move || {
        let mut seen = seen;
        while !watch_stop.load(Ordering::Relaxed) {
            if let Err(error) = print_new_inbox_messages(
                &watch_session,
                &watch_instance,
                &mut seen,
                Some(&prompt_label),
            ) {
                eprintln!("watch error: {error}");
            }
            thread::sleep(Duration::from_millis(600));
        }
    });

    loop {
        print!("{instance_label}> ");
        io::stdout().flush().context("failed to flush stdout")?;

        let mut line = String::new();
        let read = io::stdin()
            .read_line(&mut line)
            .context("failed to read stdin")?;
        if read == 0 {
            break;
        }

        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        if line == "/quit" || line == "/exit" {
            break;
        }

        if line == "/help" {
            print_term_help();
            continue;
        }

        let tokens = split_command(line);
        if tokens.is_empty() {
            continue;
        }

        match tokens[0].as_str() {
            "/msg" => {
                if tokens.len() < 3 {
                    println!("usage: /msg <instance> <message...>");
                    continue;
                }
                let target_selector = &tokens[1];
                let body = tokens[2..].join(" ");
                let (to_label, body) = with_locked_runtime_mut(|runtime| {
                    let session_id = resolve_session_selector(runtime, session_selector)?;
                    let from = resolve_instance_selector(runtime, &session_id, instance_selector)?;
                    let to = resolve_instance_selector(runtime, &session_id, target_selector)?;
                    let result = runtime.dispatch(RuntimeCommand::PostMessage {
                        session_id,
                        from,
                        route: MessageRoute::Direct { to },
                        kind: MessageKind::Chat,
                        body,
                        reply_to: None,
                    })?;
                    let RuntimeCommandResult::Message(message) = result else {
                        bail!("unexpected runtime result");
                    };
                    let to_label =
                        display_message_route(runtime, &message.session_id, &message.route);
                    Ok((to_label, message.body))
                })?;
                println!("[you -> {to_label}] {body}");
            }
            "/all" => {
                if tokens.len() < 2 {
                    println!("usage: /all <message...>");
                    continue;
                }
                let body = tokens[1..].join(" ");
                let (route_label, body) = with_locked_runtime_mut(|runtime| {
                    let session_id = resolve_session_selector(runtime, session_selector)?;
                    let from = resolve_instance_selector(runtime, &session_id, instance_selector)?;
                    let result = runtime.dispatch(RuntimeCommand::PostMessage {
                        session_id,
                        from,
                        route: MessageRoute::Broadcast,
                        kind: MessageKind::Chat,
                        body,
                        reply_to: None,
                    })?;
                    let RuntimeCommandResult::Message(message) = result else {
                        bail!("unexpected runtime result");
                    };
                    let route_label =
                        display_message_route(runtime, &message.session_id, &message.route);
                    Ok((route_label, message.body))
                })?;
                println!("[you -> {route_label}] {body}");
            }
            "/channel" => {
                if tokens.len() < 3 {
                    println!("usage: /channel <channel> <message...>");
                    continue;
                }
                let channel_selector = &tokens[1];
                let body = tokens[2..].join(" ");
                let (route_label, body) = with_locked_runtime_mut(|runtime| {
                    let session_id = resolve_session_selector(runtime, session_selector)?;
                    let from = resolve_instance_selector(runtime, &session_id, instance_selector)?;
                    let channel_id =
                        resolve_channel_selector(runtime, &session_id, channel_selector)?;
                    let result = runtime.dispatch(RuntimeCommand::PostMessage {
                        session_id,
                        from,
                        route: MessageRoute::Channel { channel_id },
                        kind: MessageKind::Chat,
                        body,
                        reply_to: None,
                    })?;
                    let RuntimeCommandResult::Message(message) = result else {
                        bail!("unexpected runtime result");
                    };
                    let route_label =
                        display_message_route(runtime, &message.session_id, &message.route);
                    Ok((route_label, message.body))
                })?;
                println!("[you -> {route_label}] {body}");
            }
            "/inbox" => {
                if tokens.len() == 1 {
                    handle_inbox(&[session_selector.to_owned(), instance_selector.to_owned()])?;
                } else {
                    handle_inbox(&[
                        session_selector.to_owned(),
                        instance_selector.to_owned(),
                        tokens[1].clone(),
                    ])?;
                }
            }
            "/events" => {
                handle_events(&[session_selector.to_owned(), instance_selector.to_owned()])?;
            }
            "/who" => {
                handle_instance(&["list".to_owned(), session_selector.to_owned()])?;
            }
            "/channels" => {
                print_instance_channels(session_selector, instance_selector)?;
            }
            "/join" => {
                if tokens.len() != 2 {
                    println!("usage: /join <channel>");
                    continue;
                }
                let channel_selector = &tokens[1];
                let channel_name = with_locked_runtime_mut(|runtime| {
                    let session_id = resolve_session_selector(runtime, session_selector)?;
                    let instance_id =
                        resolve_instance_selector(runtime, &session_id, instance_selector)?;
                    let channel_id =
                        resolve_channel_selector(runtime, &session_id, channel_selector)?;
                    let result = runtime.dispatch(RuntimeCommand::JoinChannel {
                        instance_id,
                        channel_id: channel_id.clone(),
                    })?;
                    let RuntimeCommandResult::Instance(_) = result else {
                        bail!("unexpected runtime result");
                    };
                    let channel = runtime
                        .state()
                        .channels
                        .iter()
                        .find(|channel| channel.id == channel_id)
                        .context("channel should exist after join")?;
                    Ok(channel.name.clone())
                })?;
                println!("joined #{channel_name}");
            }
            "/leave" => {
                if tokens.len() != 2 {
                    println!("usage: /leave <channel>");
                    continue;
                }
                let channel_selector = &tokens[1];
                let channel_name = with_locked_runtime_mut(|runtime| {
                    let session_id = resolve_session_selector(runtime, session_selector)?;
                    let instance_id =
                        resolve_instance_selector(runtime, &session_id, instance_selector)?;
                    let channel_id =
                        resolve_channel_selector(runtime, &session_id, channel_selector)?;
                    let channel_name = runtime
                        .state()
                        .channels
                        .iter()
                        .find(|channel| channel.id == channel_id)
                        .map(|channel| channel.name.clone())
                        .context("channel should exist before leave")?;
                    let result = runtime.dispatch(RuntimeCommand::LeaveChannel {
                        instance_id,
                        channel_id,
                    })?;
                    let RuntimeCommandResult::Instance(_) = result else {
                        bail!("unexpected runtime result");
                    };
                    Ok(channel_name)
                })?;
                println!("left #{channel_name}");
            }
            _ => run_local_command(line)?,
        }
    }

    stop.store(true, Ordering::Relaxed);
    let _ = watcher.join();
    Ok(())
}

fn run_demo() -> Result<()> {
    let mut runtime = RuntimeSupervisor::new();
    runtime.enqueue_command(RuntimeCommand::CreateSession {
        name: "bootstrap".to_owned(),
        namespace: Some(runtime_namespace()),
    });
    let session = match runtime
        .step()
        .expect("queued session command should exist")?
    {
        RuntimeCommandResult::Session(session) => session,
        other => bail!("unexpected runtime result: {other:?}"),
    };
    runtime.enqueue_command(RuntimeCommand::CreateInstance {
        session_id: session.id.clone(),
        name: "main-shell".to_owned(),
        parent_instance_id: None,
    });
    let shell = match runtime
        .step()
        .expect("queued instance command should exist")?
    {
        RuntimeCommandResult::Instance(instance) => instance,
        other => bail!("unexpected runtime result: {other:?}"),
    };
    runtime.enqueue_command(RuntimeCommand::CreateInstance {
        session_id: session.id.clone(),
        name: "build-worker".to_owned(),
        parent_instance_id: None,
    });
    let process_owner = match runtime
        .step()
        .expect("queued process owner instance command should exist")?
    {
        RuntimeCommandResult::Instance(instance) => instance,
        other => bail!("unexpected runtime result: {other:?}"),
    };

    let request = RunRequest {
        program: "powershell".to_owned(),
        args: Vec::new(),
        cwd: None,
        run_mode: RunMode::Auto,
        interactive: true,
        allow_child_instance: true,
    };
    let plan = runtime.plan_run_request(&request);
    runtime.enqueue_command(RuntimeCommand::PostMessage {
        session_id: session.id.clone(),
        from: shell.id.clone(),
        route: MessageRoute::Broadcast,
        kind: MessageKind::System,
        body: "runtime initialized".to_owned(),
        reply_to: None,
    });
    runtime.enqueue_command(RuntimeCommand::SubmitRunRequest {
        instance_id: shell.id.clone(),
        title: "open shell".to_owned(),
        run_request: request,
    });
    let mut drained = runtime.drain_commands().into_iter();
    let _message = drained.next().context("message result should exist")??;
    let job = match drained.next().context("job result should exist")?? {
        RuntimeCommandResult::Job(job) => job,
        other => bail!("unexpected runtime result: {other:?}"),
    };

    let process_request = RunRequest {
        program: "powershell".to_owned(),
        args: vec![
            "-NoProfile".to_owned(),
            "-Command".to_owned(),
            "Write-Output 'process worker line 1'; Write-Output 'process worker line 2'".to_owned(),
        ],
        cwd: None,
        run_mode: RunMode::Process,
        interactive: false,
        allow_child_instance: false,
    };
    let process_job = match runtime.dispatch(RuntimeCommand::SubmitRunRequest {
        instance_id: process_owner.id.clone(),
        title: "run process worker demo".to_owned(),
        run_request: process_request,
    })? {
        RuntimeCommandResult::Job(job) => job,
        other => bail!("unexpected runtime result: {other:?}"),
    };
    let mut process_handle = ProcessWorker::start(&process_job)?;
    for report in process_handle.drain_reports() {
        let _event = runtime.apply_worker_report(report)?;
    }
    for report in process_handle.wait()? {
        let _event = runtime.apply_worker_report(report)?;
    }

    runtime.enqueue_command(RuntimeCommand::PromoteJobToChildInstance {
        job_id: job.id.clone(),
        child_name: "shell-child".to_owned(),
    });
    runtime.apply_worker_report(WorkerReport::JobStateChanged {
        job_id: job.id.clone(),
        state: JobState::Running,
    })?;
    let _stdout = runtime.apply_worker_report(WorkerReport::Stdout {
        job_id: job.id.clone(),
        chunk: "shell worker booting".to_owned(),
    })?;
    runtime.apply_worker_report(WorkerReport::Exited {
        job_id: job.id.clone(),
        success: true,
    })?;
    let mut lifecycle = runtime.drain_commands().into_iter();
    let child = match lifecycle
        .next()
        .context("child instance result should exist")??
    {
        RuntimeCommandResult::Instance(instance) => instance,
        other => bail!("unexpected runtime result: {other:?}"),
    };

    let mailbox_size = runtime.mailbox_for_instance(&session.id, &shell.id)?.len();
    let shell_event_count = runtime.events_for_instance(&session.id, &shell.id)?.len();
    let drained_events = runtime.drain_events();
    let queued_after = runtime.queued_command_count();

    println!(
        "hc: sessions={}, instances={}, jobs={}, events_drained={}, mailbox={}, shell_events={}, queued={}, child_disposition={:?}, child={}, pty_state={:?}, process_state={:?}",
        runtime.state().sessions.len(),
        runtime.state().instances.len(),
        runtime.state().jobs.len(),
        drained_events.len(),
        mailbox_size,
        shell_event_count,
        queued_after,
        plan.child_instance,
        child.id,
        runtime.state().jobs.first().map(|job| &job.state),
        runtime.state().jobs.get(1).map(|job| &job.state),
    );

    Ok(())
}

fn reset_state() -> Result<()> {
    let _lock = acquire_state_lock()?;
    let path = state_path();
    let temp_path = state_temp_path();
    if path.exists() {
        fs::remove_file(&path).with_context(|| format!("failed to remove {}", path.display()))?;
    }
    if temp_path.exists() {
        fs::remove_file(&temp_path)
            .with_context(|| format!("failed to remove {}", temp_path.display()))?;
    }
    println!("reset {}", path.display());
    Ok(())
}

fn load_runtime() -> Result<RuntimeSupervisor> {
    let _lock = acquire_state_lock()?;
    load_runtime_unlocked()
}

fn load_runtime_unlocked() -> Result<RuntimeSupervisor> {
    let path = state_path();
    if !path.exists() {
        return Ok(RuntimeSupervisor::new());
    }

    let content =
        fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
    let state: RuntimeState = serde_json::from_str(&content)
        .with_context(|| format!("failed to parse {}", path.display()))?;
    Ok(RuntimeSupervisor::from_state(state))
}

fn save_runtime_unlocked(runtime: &RuntimeSupervisor) -> Result<()> {
    let path = state_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let temp_path = state_temp_path();
    let json = serde_json::to_string_pretty(runtime.state())
        .context("failed to serialize runtime state")?;
    fs::write(&temp_path, json)
        .with_context(|| format!("failed to write {}", temp_path.display()))?;
    if path.exists() {
        fs::remove_file(&path).with_context(|| format!("failed to replace {}", path.display()))?;
    }
    fs::rename(&temp_path, &path).with_context(|| {
        format!(
            "failed to rename {} to {}",
            temp_path.display(),
            path.display()
        )
    })?;
    Ok(())
}

fn with_locked_runtime_mut<T, F>(mutator: F) -> Result<T>
where
    F: FnOnce(&mut RuntimeSupervisor) -> Result<T>,
{
    let _lock = acquire_state_lock()?;
    let mut runtime = load_runtime_unlocked()?;
    let value = mutator(&mut runtime)?;
    save_runtime_unlocked(&runtime)?;
    Ok(value)
}

fn state_path() -> PathBuf {
    if let Ok(path) = env::var("HC_RUNTIME_STATE_PATH") {
        return PathBuf::from(path);
    }

    let namespace = runtime_namespace();

    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("workspace")
        .join("tenants")
        .join(namespace.tenant_id)
        .join("users")
        .join(namespace.user_id)
        .join("indexes")
        .join("runtime-state.json")
}

fn runtime_namespace() -> RuntimeNamespace {
    let tenant_id = env::var("HC_TENANT_ID").unwrap_or_else(|_| "local".to_owned());
    let user_id = env::var("HC_USER_ID").unwrap_or_else(|_| "default".to_owned());
    RuntimeNamespace::new(tenant_id, user_id)
}

fn state_temp_path() -> PathBuf {
    state_path().with_extension("json.tmp")
}

fn state_lock_path() -> PathBuf {
    state_path().with_extension("json.lock")
}

struct StateLockGuard {
    path: PathBuf,
}

impl Drop for StateLockGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn acquire_state_lock() -> Result<StateLockGuard> {
    let path = state_lock_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }

    for _ in 0..200 {
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(_) => return Ok(StateLockGuard { path }),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                thread::sleep(Duration::from_millis(25));
            }
            Err(error) => {
                return Err(error).with_context(|| format!("failed to acquire {}", path.display()));
            }
        }
    }

    bail!("timed out waiting for state lock: {}", path.display())
}

fn resolve_session_selector(runtime: &RuntimeSupervisor, selector: &str) -> Result<String> {
    if let Some(session) = runtime
        .state()
        .sessions
        .iter()
        .find(|session| session.id == selector || session.name == selector)
    {
        return Ok(session.id.clone());
    }

    bail!("session not found: {selector}")
}

fn resolve_instance_selector(
    runtime: &RuntimeSupervisor,
    session_id: &str,
    selector: &str,
) -> Result<String> {
    if let Some(instance) = runtime.state().instances.iter().find(|instance| {
        instance.session_id == session_id && (instance.id == selector || instance.name == selector)
    }) {
        return Ok(instance.id.clone());
    }

    bail!("instance not found in {session_id}: {selector}")
}

fn resolve_channel_selector(
    runtime: &RuntimeSupervisor,
    session_id: &str,
    selector: &str,
) -> Result<String> {
    if let Some(channel) = runtime.state().channels.iter().find(|channel| {
        channel.session_id == session_id && (channel.id == selector || channel.name == selector)
    }) {
        return Ok(channel.id.clone());
    }

    bail!("channel not found in {session_id}: {selector}")
}

fn print_instance_channels(session_selector: &str, instance_selector: &str) -> Result<()> {
    let runtime = load_runtime()?;
    let session_id = resolve_session_selector(&runtime, session_selector)?;
    let instance_id = resolve_instance_selector(&runtime, &session_id, instance_selector)?;
    let instance = runtime
        .state()
        .instances
        .iter()
        .find(|instance| instance.id == instance_id)
        .context("instance should exist")?;

    if instance.channel_ids.is_empty() {
        println!("no joined channels");
        return Ok(());
    }

    for channel_id in &instance.channel_ids {
        let label = runtime
            .state()
            .channels
            .iter()
            .find(|channel| channel.id == *channel_id)
            .map(|channel| format!("{} {}", channel.id, channel.name))
            .unwrap_or_else(|| channel_id.clone());
        println!("{label}");
    }

    Ok(())
}

fn message_matches_filter(
    runtime: &RuntimeSupervisor,
    session_id: &str,
    message: &hc_core::MessageRecord,
    route_filter: Option<&str>,
) -> Result<bool> {
    let Some(route_filter) = route_filter else {
        return Ok(true);
    };

    if route_filter == "*" || route_filter.eq_ignore_ascii_case("all") {
        return Ok(matches!(message.route, MessageRoute::Broadcast));
    }

    match &message.route {
        MessageRoute::Direct { to } => {
            Ok(route_filter == to || route_filter == display_instance(runtime, session_id, to))
        }
        MessageRoute::Channel { channel_id } => {
            let channel = runtime
                .state()
                .channels
                .iter()
                .find(|channel| channel.id == *channel_id);
            Ok(route_filter == channel_id
                || channel
                    .map(|channel| {
                        route_filter == channel.name || route_filter == format!("#{}", channel.name)
                    })
                    .unwrap_or(false))
        }
        MessageRoute::Broadcast => Ok(false),
    }
}

fn watch_inbox(session_selector: &str, instance_selector: &str) -> Result<()> {
    println!("watching inbox for {instance_selector} in {session_selector}. Press Ctrl+C to stop.");
    let mut seen = 0usize;

    loop {
        let runtime = load_runtime()?;
        let session_id = resolve_session_selector(&runtime, session_selector)?;
        let instance_id = resolve_instance_selector(&runtime, &session_id, instance_selector)?;
        let messages = runtime.mailbox_for_instance(&session_id, &instance_id)?;

        for message in messages.iter().skip(seen) {
            let from_label = display_instance(&runtime, &session_id, &message.from);
            println!("[{}] {} -> you: {}", message.id, from_label, message.body);
        }

        seen = messages.len();
        thread::sleep(Duration::from_millis(600));
    }
}

fn print_new_inbox_messages(
    session_selector: &str,
    instance_id: &str,
    seen: &mut usize,
    prompt_label: Option<&str>,
) -> Result<()> {
    let runtime = load_runtime()?;
    let session_id = resolve_session_selector(&runtime, session_selector)?;
    let messages = runtime.mailbox_for_instance(&session_id, instance_id)?;
    let mut printed_any = false;

    for message in messages.iter().skip(*seen) {
        let from_label = display_instance(&runtime, &session_id, &message.from);
        println!(
            "[recv {}] {} -> you: {}",
            message.id, from_label, message.body
        );
        printed_any = true;
    }

    *seen = messages.len();
    if printed_any {
        if let Some(prompt_label) = prompt_label {
            print!("{prompt_label}> ");
            io::stdout().flush().context("failed to flush stdout")?;
        }
    }
    Ok(())
}

fn watch_events(session_selector: &str, instance_selector: Option<&str>) -> Result<()> {
    match instance_selector {
        Some(instance_selector) => {
            println!(
                "watching events for {instance_selector} in {session_selector}. Press Ctrl+C to stop."
            );
        }
        None => {
            println!("watching events for {session_selector}. Press Ctrl+C to stop.");
        }
    }

    let mut seen = 0usize;

    loop {
        let runtime = load_runtime()?;
        let session_id = resolve_session_selector(&runtime, session_selector)?;

        if let Some(instance_selector) = instance_selector {
            let instance_id = resolve_instance_selector(&runtime, &session_id, instance_selector)?;
            let events = runtime.events_for_instance(&session_id, &instance_id)?;

            for event in events.iter().skip(seen) {
                println!(
                    "[{}] {:?} source={} target={} payload={}",
                    event.id,
                    event.kind,
                    event.source,
                    event.target.as_deref().unwrap_or("-"),
                    event.payload.trim_end()
                );
            }

            seen = events.len();
        } else {
            let events: Vec<_> = runtime
                .state()
                .events
                .iter()
                .filter(|event| event.session_id == session_id)
                .collect();

            for event in events.iter().skip(seen) {
                println!(
                    "[{}] {:?} source={} target={} payload={}",
                    event.id,
                    event.kind,
                    event.source,
                    event.target.as_deref().unwrap_or("-"),
                    event.payload.trim_end()
                );
            }

            seen = events.len();
        }

        thread::sleep(Duration::from_millis(600));
    }
}

fn print_help() {
    println!("hc demo");
    println!("hc reset");
    println!("hc session create <name>");
    println!("hc session list");
    println!("hc instance create <session> <name>");
    println!("hc instance list <session>");
    println!("hc channel create <session> <name>");
    println!("hc channel list <session>");
    println!("hc channel join <session> <instance> <channel>");
    println!("hc channel leave <session> <instance> <channel>");
    println!("hc channel send <session> <from_instance> <channel> <message...>");
    println!("hc send <session> <from_instance> <to_instance> <message...>");
    println!("hc send --all <session> <from_instance> <message...>");
    println!("hc claim submit <session> <instance> <message_id> <score> [reason...]");
    println!("hc claim list <message_id>");
    println!("hc claim resolve <message_id> <round>");
    println!("hc term <session> <instance>");
    println!("hc chat <session> <from_instance> <to_instance>");
    println!("hc inbox <session> <instance> [route]");
    println!("hc events <session> [instance]");
    println!("hc watch inbox <session> <instance>");
    println!("hc watch events <session> [instance]");
}

fn print_control_help() {
    println!("/session create <name>");
    println!("/session list");
    println!("/instance create <session> <name>");
    println!("/instance list <session>");
    println!("/channel create <session> <name>");
    println!("/channel list <session>");
    println!("/channel join <session> <instance> <channel>");
    println!("/channel leave <session> <instance> <channel>");
    println!("/claim submit <session> <instance> <message_id> <score> [reason...]");
    println!("/claim list <message_id>");
    println!("/claim resolve <message_id> <round>");
    println!("/term <session> <instance>");
    println!("/reset");
    println!("/quit");
}

fn print_term_help() {
    println!("/msg <instance> <message...>");
    println!("/all <message...>");
    println!("/channel <channel> <message...>");
    println!("/inbox [route]");
    println!("/events");
    println!("/who");
    println!("/channels");
    println!("/join <channel>");
    println!("/leave <channel>");
    println!("/quit");
}

fn display_instance(runtime: &RuntimeSupervisor, session_id: &str, instance_id: &str) -> String {
    runtime
        .state()
        .instances
        .iter()
        .find(|instance| instance.session_id == session_id && instance.id == instance_id)
        .map(|instance| instance.name.clone())
        .unwrap_or_else(|| instance_id.to_owned())
}

fn display_message_route(
    runtime: &RuntimeSupervisor,
    session_id: &str,
    route: &MessageRoute,
) -> String {
    match route {
        MessageRoute::Direct { to } => display_instance(runtime, session_id, to),
        MessageRoute::Broadcast => "*".to_owned(),
        MessageRoute::Channel { channel_id } => runtime
            .state()
            .channels
            .iter()
            .find(|channel| channel.session_id == session_id && channel.id == *channel_id)
            .map(|channel| format!("#{}", channel.name))
            .unwrap_or_else(|| format!("#{}", channel_id)),
    }
}

fn split_command(line: &str) -> Vec<String> {
    line.split_whitespace().map(ToOwned::to_owned).collect()
}

fn current_timestamp_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

fn run_local_command(line: &str) -> Result<()> {
    let mut child = Command::new("powershell")
        .args(["-NoProfile", "-Command", line])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("failed to run local command: {line}"))?;

    let stdout = child
        .stdout
        .take()
        .context("failed to capture local command stdout")?;
    let stderr = child
        .stderr
        .take()
        .context("failed to capture local command stderr")?;

    let stdout_thread = thread::spawn(move || -> io::Result<()> {
        let reader = BufReader::new(stdout);
        for line in reader.lines() {
            println!("{}", line?);
        }
        Ok(())
    });

    let stderr_thread = thread::spawn(move || -> io::Result<()> {
        let reader = BufReader::new(stderr);
        for line in reader.lines() {
            eprintln!("{}", line?);
        }
        Ok(())
    });

    let status = child
        .wait()
        .with_context(|| format!("failed to wait for local command: {line}"))?;

    stdout_thread
        .join()
        .map_err(|_| anyhow::anyhow!("stdout reader thread panicked"))?
        .context("failed while reading local command stdout")?;
    stderr_thread
        .join()
        .map_err(|_| anyhow::anyhow!("stderr reader thread panicked"))?
        .context("failed while reading local command stderr")?;

    if !status.success() {
        println!("command exited with {status}");
    }

    Ok(())
}
