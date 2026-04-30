use super::{
    McpServerRepository, McpServerSpec, ToolExecutionKind, ToolExecutionOutcome, ToolProvider,
    ToolRepository, ToolSpec, ToolStability, build_default_tool_execution_plan, builtin_tool,
    default_tool_catalog, default_tool_command, discover_mcp_tools_with_timeout,
    is_mcp_tool_command, mcp_tool_id, normalize_mcp_server_id, seed_tool_local_dir_list,
    seed_tool_local_file_read, seed_tool_rg,
};
use hc_capability::ModelDependence;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[test]
fn default_catalog_exposes_builtin_tools() {
    let catalog = default_tool_catalog();
    assert!(catalog.contains("tool.rg"));
    assert!(catalog.contains("tool.cargo-test"));
    assert!(catalog.contains("tool.local-file.read"));
    assert!(catalog.contains("tool.local-file.write"));
    assert!(catalog.contains("tool.local-dir.list"));
    assert_eq!(catalog.list().len(), 5);
}

#[test]
fn rg_default_command_uses_declared_search_mode() {
    let tool = seed_tool_rg();
    let command = default_tool_command(&tool, "which file defines ToolSpec");
    assert_eq!(command, vec!["rg".to_owned(), "-n".to_owned()]);
}

#[test]
fn builtin_tool_lookup_returns_registered_tool() {
    let tool = builtin_tool("tool.local-file.read").expect("builtin local file read should exist");
    assert_eq!(tool.id, "tool.local-file.read");
    assert!(default_tool_catalog().get_tool(&tool.id).is_some());
}

#[test]
fn wrapped_outcome_keeps_invoked_tool_chain() {
    let outcome = ToolExecutionOutcome {
        tool_id: "tool.rg".to_owned(),
        parent_tool_id: None,
        invoked_tool_ids: Vec::new(),
        goal: "find docs".to_owned(),
        command: vec!["rg".to_owned(), "-n".to_owned(), "docs".to_owned()],
        success: true,
        summary: "ok".to_owned(),
        observations: vec!["docs/working-rules.md".to_owned()],
    }
    .wrapped_by("tool.docs.lookup");

    assert_eq!(outcome.tool_id, "tool.docs.lookup");
    assert_eq!(outcome.parent_tool_id, None);
    assert_eq!(outcome.invoked_tool_ids, vec!["tool.rg".to_owned()]);
}

#[test]
fn default_plan_keeps_toolchain_planning_decoupled_from_apps() {
    let tool = seed_tool_rg();
    let plan = build_default_tool_execution_plan(&tool, "which file defines ToolSpec")
        .expect("default plan should build");

    assert_eq!(plan.tool_id, "tool.rg");
    assert_eq!(
        plan.suggested_command,
        vec!["rg".to_owned(), "-n".to_owned()]
    );
    assert!(!plan.guidance.is_empty());
    assert!(!plan.validation_steps.is_empty());
    assert!(!plan.recovery_steps.is_empty());
}

#[test]
fn default_plan_supports_core_local_file_tools() {
    let tool = seed_tool_local_file_read();
    let plan = build_default_tool_execution_plan(&tool, "read README").expect("plan should build");

    assert_eq!(plan.tool_id, "tool.local-file.read");
    assert_eq!(
        plan.suggested_command,
        vec!["hc.local-file.read".to_owned()]
    );
    assert!(!plan.guidance.is_empty());
    assert!(!plan.validation_steps.is_empty());
}

#[test]
fn default_plan_supports_core_local_dir_tools() {
    let tool = seed_tool_local_dir_list();
    let plan = build_default_tool_execution_plan(&tool, "list src").expect("plan should build");

    assert_eq!(plan.tool_id, "tool.local-dir.list");
    assert_eq!(plan.suggested_command, vec!["hc.local-dir.list".to_owned()]);
    assert!(!plan.guidance.is_empty());
}

#[test]
fn tool_repository_roundtrips_markdown_tool() {
    let root = unique_temp_dir("tool-repo");
    let repository = ToolRepository::new(&root);
    let tool = ToolSpec {
        id: "tool.echo".to_owned(),
        name: "Echo".to_owned(),
        description: "Echoes text with printf.".to_owned(),
        execution_kind: ToolExecutionKind::Cli,
        composition: super::ToolComposition::Atomic,
        stability: ToolStability::Managed,
        model_dependence: ModelDependence::Optional,
        default_command: vec!["printf".to_owned()],
        tags: vec!["tool".to_owned(), "shell".to_owned()],
    };

    let path = repository.write_tool(&tool).expect("tool should write");
    assert!(path.ends_with("tools/tool.echo.md"));

    let loaded = repository
        .load_catalog()
        .expect("tool catalog should load")
        .get("tool.echo")
        .cloned()
        .expect("custom tool should be present");
    assert_eq!(loaded, tool);
}

#[test]
fn mcp_server_repository_roundtrips_markdown_server() {
    let root = unique_temp_dir("mcp-server-repo");
    let repository = McpServerRepository::new(&root);
    let server = McpServerSpec {
        id: "mcp.echo".to_owned(),
        name: "Echo MCP".to_owned(),
        description: "Exposes echo tools over MCP.".to_owned(),
        transport: hc_toolchain::McpTransportKind::Stdio,
        url: None,
        command: vec!["python3".to_owned(), "echo_server.py".to_owned()],
        tags: vec!["mcp".to_owned(), "echo".to_owned()],
    };

    let path = repository
        .write_server(&server)
        .expect("mcp server should write");
    assert!(path.ends_with("mcp/servers/mcp.echo.md"));

    let loaded = repository
        .get_server("echo")
        .expect("mcp server should load by short id");
    assert_eq!(loaded, server);
}

#[test]
fn mcp_tool_command_identity_is_structural() {
    assert_eq!(normalize_mcp_server_id("echo"), "mcp.echo");
    assert_eq!(
        mcp_tool_id("mcp.echo", "say hello"),
        "tool.mcp.echo.say-hello"
    );
    assert!(is_mcp_tool_command(&[
        "hc.mcp.call".to_owned(),
        "mcp.echo".to_owned(),
        "say hello".to_owned()
    ]));
}

#[test]
fn mcp_discovery_times_out_when_server_does_not_respond() {
    let server = McpServerSpec {
        id: "mcp.sleepy".to_owned(),
        name: "Sleepy MCP".to_owned(),
        description: "Never responds during discovery.".to_owned(),
        transport: hc_toolchain::McpTransportKind::Stdio,
        url: None,
        command: vec!["sh".to_owned(), "-c".to_owned(), "sleep 5".to_owned()],
        tags: vec!["mcp".to_owned()],
    };

    let started = Instant::now();
    let error = discover_mcp_tools_with_timeout(&server, Duration::from_millis(50))
        .expect_err("discovery should time out");

    assert!(started.elapsed() < Duration::from_secs(2));
    assert!(error.to_string().contains("timed out"));
}

fn unique_temp_dir(label: &str) -> PathBuf {
    let suffix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("time should be after unix epoch")
        .as_nanos();
    std::env::temp_dir().join(format!("hc-{label}-{suffix}"))
}
