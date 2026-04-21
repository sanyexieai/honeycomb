use std::{
    env,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use hc_responder::HumanInboxRepository;
use hc_store::store::WorkspaceNamespace;

fn main() -> Result<()> {
    let args: Vec<String> = env::args().skip(1).collect();
    match args.as_slice() {
        [] => {
            print_help();
            Ok(())
        }
        [cmd] if cmd == "help" || cmd == "--help" || cmd == "-h" => {
            print_help();
            Ok(())
        }
        [scope, action, rest @ ..] if scope == "inbox" => handle_inbox(action, rest),
        [other, ..] => bail!("unknown command: {other}"),
    }
}

fn handle_inbox(action: &str, args: &[String]) -> Result<()> {
    let namespace = parse_namespace(args)?;
    let repo = HumanInboxRepository::with_namespace(workspace_root(), namespace);

    match action {
        "list" => {
            let items = repo.list_pending()?;
            if items.is_empty() {
                println!("no pending human responder items");
                return Ok(());
            }
            for item in items {
                println!(
                    "{} | as {} ({}) | from {} | {}",
                    item.id,
                    item.replying_agent_name,
                    item.replying_role,
                    item.source_from_instance_id,
                    item.source_body
                );
            }
            Ok(())
        }
        "show" => {
            let item_id = first_non_flag_arg(args).context("missing item id for inbox show")?;
            let item = repo.read_pending(&item_id)?;
            println!("id: {}", item.id);
            println!(
                "reply as: {} ({})",
                item.replying_agent_name, item.replying_role
            );
            println!("from: {}", item.source_from_instance_id);
            println!("session: {}", item.source_session_id);
            println!("message: {}", item.source_message_id);
            println!();
            println!("{}", item.source_body);
            Ok(())
        }
        "reply" => {
            let (item_id, body) = parse_reply_args(args)
                .context("usage: hc-responder-cli inbox reply <item-id> <text...>")?;
            repo.mark_answered(&item_id, body, current_timestamp_ms())?;
            println!("answered {item_id}");
            Ok(())
        }
        "reply-next" => {
            let body = parse_reply_next_args(args)
                .context("usage: hc-responder-cli inbox reply-next <text...>")?;
            let item = repo
                .list_pending()?
                .into_iter()
                .next()
                .context("no pending human responder items")?;
            repo.mark_answered(&item.id, body, current_timestamp_ms())?;
            println!(
                "answered {} | as {} ({}) | from {}",
                item.id, item.replying_agent_name, item.replying_role, item.source_from_instance_id
            );
            Ok(())
        }
        other => bail!("unknown inbox action: {other}"),
    }
}

fn parse_namespace(args: &[String]) -> Result<WorkspaceNamespace> {
    let mut tenant_id = env::var("HC_TENANT_ID").unwrap_or_else(|_| "local".to_owned());
    let mut user_id = env::var("HC_USER_ID").unwrap_or_else(|_| "default".to_owned());

    let mut index = 0usize;
    while index < args.len() {
        match args[index].as_str() {
            "--tenant" => {
                tenant_id = args
                    .get(index + 1)
                    .cloned()
                    .context("missing value for --tenant")?;
                index += 2;
            }
            "--user" => {
                user_id = args
                    .get(index + 1)
                    .cloned()
                    .context("missing value for --user")?;
                index += 2;
            }
            _ => index += 1,
        }
    }

    Ok(WorkspaceNamespace::new(tenant_id, user_id))
}

fn first_non_flag_arg(args: &[String]) -> Option<String> {
    let mut skip_next = false;
    for arg in args {
        if skip_next {
            skip_next = false;
            continue;
        }
        if arg == "--tenant" || arg == "--user" {
            skip_next = true;
            continue;
        }
        if !arg.starts_with("--") {
            return Some(arg.clone());
        }
    }
    None
}

fn parse_reply_args(args: &[String]) -> Option<(String, String)> {
    let mut filtered = Vec::new();
    let mut skip_next = false;
    for arg in args {
        if skip_next {
            skip_next = false;
            continue;
        }
        if arg == "--tenant" || arg == "--user" {
            skip_next = true;
            continue;
        }
        filtered.push(arg.clone());
    }
    if filtered.len() < 2 {
        return None;
    }
    Some((filtered[0].clone(), filtered[1..].join(" ")))
}

fn parse_reply_next_args(args: &[String]) -> Option<String> {
    let mut filtered = Vec::new();
    let mut skip_next = false;
    for arg in args {
        if skip_next {
            skip_next = false;
            continue;
        }
        if arg == "--tenant" || arg == "--user" {
            skip_next = true;
            continue;
        }
        filtered.push(arg.clone());
    }
    if filtered.is_empty() {
        return None;
    }
    Some(filtered.join(" "))
}

fn current_timestamp_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

fn workspace_root() -> PathBuf {
    PathBuf::from("workspace")
}

fn print_help() {
    println!("hc-responder-cli inbox list [--tenant <id>] [--user <id>]");
    println!("hc-responder-cli inbox show <item-id> [--tenant <id>] [--user <id>]");
    println!("hc-responder-cli inbox reply <item-id> <text...> [--tenant <id>] [--user <id>]");
    println!("hc-responder-cli inbox reply-next <text...> [--tenant <id>] [--user <id>]");
}
