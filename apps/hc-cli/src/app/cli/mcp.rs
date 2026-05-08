//! `hc-cli mcp` 子命令。
use anyhow::{Result, bail};
use hc_toolchain::{McpServerSpec, McpTransportKind, call_mcp_tool, normalize_mcp_server_id};
use std::collections::BTreeMap;

pub(super) fn handle_mcp(args: &[String]) -> Result<()> {
    match args {
        [cmd, rest @ ..] if cmd == "add" => handle_mcp_add(rest),
        [cmd, rest @ ..] if cmd == "list" => handle_mcp_list(rest),
        [cmd, rest @ ..] if cmd == "tools" => handle_mcp_tools(rest),
        [cmd, rest @ ..] if cmd == "call" => handle_mcp_call(rest),
        [] => bail!("usage: hc-cli mcp <add|list|tools|call> ..."),
        [other, ..] => bail!("unknown mcp command: {other}"),
    }
}

fn handle_mcp_add(args: &[String]) -> Result<()> {
    let options = super::parse_mcp_add_options(args)?;
    let transport = options.transport.unwrap_or_else(|| {
        if options.url.is_some() {
            McpTransportKind::StreamableHttp
        } else {
            McpTransportKind::Stdio
        }
    });
    let server = McpServerSpec {
        id: normalize_mcp_server_id(&options.id),
        name: options.name,
        description: options.description,
        enabled: true,
        transport,
        url: options.url,
        command: options.command,
        default_args: BTreeMap::new(),
        tags: super::normalized_tags(options.tags, "mcp"),
    };
    let path = super::mcp_server_repository().write_server(&server)?;

    if options.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "server": server,
                "path": path,
            }))?
        );
        return Ok(());
    }

    println!("mcp> {}", server.id);
    println!("path> {}", path.display());
    println!("transport> {:?}", server.transport);
    if let Some(url) = &server.url {
        println!("url> {url}");
    }
    if !server.command.is_empty() {
        println!("command> {}", server.command.join(" "));
    }
    Ok(())
}

fn handle_mcp_list(args: &[String]) -> Result<()> {
    let options = super::parse_common_options(args)?;
    let servers = super::mcp_server_repository().list_servers()?;
    if options.json {
        println!("{}", serde_json::to_string_pretty(&servers)?);
        return Ok(());
    }
    for server in servers {
        let endpoint = server
            .url
            .clone()
            .unwrap_or_else(|| server.command.join(" "));
        println!(
            "{} | {} | {} | {:?} | {}",
            server.id,
            server.name,
            if server.enabled {
                "enabled"
            } else {
                "disabled"
            },
            server.transport,
            endpoint
        );
    }
    Ok(())
}

fn handle_mcp_tools(args: &[String]) -> Result<()> {
    let options = super::parse_common_options(args)?;
    let servers = super::mcp_server_repository().list_servers()?;
    let mut tools = Vec::new();
    for server in servers {
        if !server.enabled {
            continue;
        }
        let cache = super::mcp_server_repository().refresh_tool_cache(&server)?;
        tools.extend(cache.tools);
    }
    if options.json {
        println!("{}", serde_json::to_string_pretty(&tools)?);
        return Ok(());
    }
    for tool in tools {
        println!("{} | {} | {}", tool.id, tool.name, tool.description);
    }
    Ok(())
}

fn handle_mcp_call(args: &[String]) -> Result<()> {
    if args.len() < 2 {
        bail!("usage: hc-cli mcp call <server-id> <tool-name> [key=value ...] [--json]");
    }
    let mut json_output = false;
    let server_id = args[0].clone();
    let tool_name = args[1].clone();
    let mut call_args = Vec::new();
    for arg in &args[2..] {
        if arg == "--json" {
            json_output = true;
        } else {
            call_args.push(arg.clone());
        }
    }
    let server = super::mcp_server_repository().get_server(&server_id)?;
    let mut arguments = super::arguments_from_run_args(&call_args, None)?;
    super::insert_missing_platform_mcp_runtime_arguments(&mut arguments);
    let result = call_mcp_tool(&server, &tool_name, serde_json::Value::Object(arguments))?;
    if json_output {
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        super::print_mcp_result(&result);
    }
    Ok(())
}
