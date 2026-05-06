use super::{
    KeywordToolSelector, ToolSelector, build_chat_request_history, build_from_create_tool_command,
    code_block_extension, execute_builtin_tool, expand_default_command_token_in_root,
    extract_code_blocks, extract_create_tool_command, looks_like_complete_artifact,
    parse_tool_build_response, parse_tool_route_response, render_chat_error,
    render_tool_execution_context, render_tool_selection_context, sanitize_model_response,
    score_tool_for_goal, selection_input_from_history, skill_from_natural_language_draft,
    tool_from_natural_language_draft, try_execute_create_tool_command_from_response,
    write_generated_tool_files_under,
};
use hc_service::timed_turn::{
    TimedSequenceRule, extract_i64_numbers, reminder_delay_seconds, timed_sequence_end,
};
use hc_capability::ModelDependence;
use hc_llm::{ChatMessage, LlmError, MessageRole};
use hc_toolchain::{
    ToolCatalog, ToolComposition, ToolExecutionKind, ToolExecutionPlan, ToolSpec, ToolStability,
};
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

#[test]
fn selects_created_frontend_tool_for_chinese_page_request() {
    let tool = ToolSpec {
        id: "tool.fe-red".to_owned(),
        name: "前端工程师红色版".to_owned(),
        description: "前端工程师红色系技能工具包，包含 React/Vue/TypeScript 开发能力".to_owned(),
        execution_kind: ToolExecutionKind::Cli,
        composition: ToolComposition::Atomic,
        stability: ToolStability::Managed,
        model_dependence: ModelDependence::Optional,
        default_command: vec!["echo red".to_owned()],
        tags: vec!["frontend".to_owned(), "red-theme".to_owned()],
    };
    let mut catalog = ToolCatalog::new();
    catalog.register(tool.clone());

    assert!(score_tool_for_goal(&tool, "写一个前端登陆页面") > 0);
    let selector = KeywordToolSelector::default();
    let selection = selector
        .select("写一个前端登陆页面", &catalog)
        .expect("tool selection should run");
    let context = render_tool_selection_context(&selection)
        .expect("frontend tool context should be selected");
    assert!(context.contains("tool.fe-red"));
    assert!(context.contains("红色系"));
}

#[test]
fn selection_context_surfaces_candidates_even_without_keyword_hit() {
    let tool = ToolSpec {
        id: "tool.frontend-red-theme".to_owned(),
        name: "前端开发红色系技能".to_owned(),
        description: "提供红色系前端开发风格指南和代码规范".to_owned(),
        execution_kind: ToolExecutionKind::Builtin,
        composition: ToolComposition::Composite,
        stability: ToolStability::Managed,
        model_dependence: ModelDependence::Optional,
        default_command: Vec::new(),
        tags: vec![
            "frontend".to_owned(),
            "red-theme".to_owned(),
            "skill".to_owned(),
        ],
    };
    let mut catalog = ToolCatalog::new();
    catalog.register(tool);

    let selector = KeywordToolSelector::default();
    let selection = selector
        .select("写一个登陆页面", &catalog)
        .expect("tool selection should run");
    let context =
        render_tool_selection_context(&selection).expect("candidate context should be available");
    assert!(context.contains("Internal tool candidates"));
    assert!(context.contains("tool.frontend-red-theme"));
}

#[test]
fn selected_tool_context_is_merged_into_single_system_message() {
    let history = vec![ChatMessage::new(MessageRole::System, "base prompt")];
    let messages =
        build_chat_request_history(&history, Some("selected tool context".to_owned()), "hello");

    assert_eq!(
        messages
            .iter()
            .filter(|message| message.role == MessageRole::System)
            .count(),
        1
    );
    assert!(messages[0].content.contains("base prompt"));
    assert!(messages[0].content.contains("selected tool context"));
    assert_eq!(messages.last().unwrap().role, MessageRole::User);
}

#[test]
fn selection_input_includes_recent_user_turns() {
    let history = vec![
        ChatMessage::new(MessageRole::System, "base prompt"),
        ChatMessage::new(MessageRole::User, "写一个前端页面"),
        ChatMessage::new(MessageRole::Assistant, "你想创建哪种类型的前端页面？"),
    ];
    let input = selection_input_from_history(&history, "登陆页");

    assert!(input.contains("写一个前端页面"));
    assert!(input.contains("登陆页"));
}

#[test]
fn invalid_chat_setting_error_gets_friendly_context() {
    let rendered = render_chat_error(&LlmError::ProviderFailure(
        "http 400: invalid chat setting (2013)".to_owned(),
    ));
    assert!(rendered.contains("provider rejected the chat request"));
    assert!(rendered.contains("/clear"));
}

#[test]
fn extracts_chinese_countdown_numbers_in_order() {
    assert_eq!(extract_i64_numbers("倒数十个数"), vec![10]);
    assert_eq!(extract_i64_numbers("从二十倒数到十五"), vec![20, 15]);
    assert_eq!(extract_i64_numbers("从10开始倒计时到0"), vec![10, 0]);
}

#[test]
fn countdown_quantity_uses_one_as_end() {
    let rule = TimedSequenceRule {
        direction: "countdown".to_owned(),
        default_end: Some(0),
        max_items: 120,
        ..TimedSequenceRule::default()
    };
    assert_eq!(timed_sequence_end("倒数十个数", 10, &[10], &rule), 1);
    assert_eq!(
        timed_sequence_end("从10开始倒计时到0", 10, &[10, 0], &rule),
        0
    );
}

#[test]
fn parses_simple_reminder_delay() {
    assert_eq!(
        reminder_delay_seconds(
            "\u{4e00}\u{5206}\u{949f}\u{4ee5}\u{540e}\u{53eb}\u{6211}",
            30
        ),
        Some(60)
    );
    assert_eq!(
        reminder_delay_seconds("remind me in 2 minutes", 30),
        Some(120)
    );
    assert_eq!(
        reminder_delay_seconds("\u{7a0d}\u{540e}\u{53eb}\u{6211}", 30),
        Some(30)
    );
}

#[test]
fn parses_tool_build_json_from_model_response() {
    let build = parse_tool_build_response(
        r#"{"action":"create_tool","message":null,"tool":{"id":"tool.echo","name":"Echo","description":"Echoes text with printf.","execution_kind":"cli","default_command":["printf"],"tags":["shell"]}}"#,
    )
    .expect("json should parse");

    let tool = tool_from_natural_language_draft(build.tool.expect("tool should exist"))
        .expect("tool should be valid");
    assert_eq!(tool.id, "tool.echo");
    assert_eq!(tool.default_command, vec!["printf".to_owned()]);
}

#[test]
fn parses_tool_builder_ignore_action() {
    let build =
        parse_tool_build_response(r#"{"action":"ignore","message":null,"tool":null,"skill":null}"#)
            .expect("ignore json should parse");

    assert_eq!(build.action, "ignore");
    assert!(build.tool.is_none());
    assert!(build.skill.is_none());
}

#[test]
fn parses_tool_route_json() {
    let route = parse_tool_route_response(
        r#"{"action":"run_tool","tool_id":"tool.local-file.read","args":["README.md"],"goal":"read README","message":null}"#,
    )
    .expect("route json should parse");

    assert_eq!(route.action, "run_tool");
    assert_eq!(route.tool_id.as_deref(), Some("tool.local-file.read"));
    assert_eq!(route.args, vec!["README.md".to_owned()]);
    assert_eq!(route.goal.as_deref(), Some("read README"));
}

#[test]
fn renders_tool_execution_context_for_llm_followup() {
    let plan = ToolExecutionPlan {
        tool_id: "tool.local-file.read".to_owned(),
        suggested_command: vec!["hc.local-file.read".to_owned(), "README.md".to_owned()],
        guidance: Vec::new(),
        validation_steps: Vec::new(),
        recovery_steps: Vec::new(),
    };
    let outcome = hc_toolchain::ToolExecutionOutcome {
        tool_id: "tool.local-file.read".to_owned(),
        parent_tool_id: None,
        invoked_tool_ids: Vec::new(),
        goal: "read README".to_owned(),
        command: plan.suggested_command.clone(),
        success: true,
        summary: "read 12 bytes".to_owned(),
        observations: vec!["content: hello".to_owned()],
    };

    let context = render_tool_execution_context(&plan, &outcome);

    assert!(context.contains("Internal execution record"));
    assert!(context.contains("tool.local-file.read"));
    assert!(context.contains("content: hello"));
}

#[test]
fn tool_builder_can_return_generated_files() {
    let build = parse_tool_build_response(
        r##"{"action":"create_tool","message":null,"tool":{"id":"tool.demo-script","name":"Demo Script","description":"Runs a generated script.","execution_kind":"script","default_command":["bash","@file:tools/bin/demo-script.sh"],"files":[{"path":"tools/bin/demo-script.sh","content":"#!/usr/bin/env bash\necho demo\n","executable":true}],"tags":["demo"]},"skill":null}"##,
    )
    .expect("tool with files should parse");
    let draft = build.tool.expect("tool draft should exist");

    assert_eq!(draft.files.len(), 1);
    assert_eq!(draft.files[0].path, "tools/bin/demo-script.sh");
    let tool = tool_from_natural_language_draft(draft).expect("tool should build");
    assert_eq!(tool.execution_kind, ToolExecutionKind::Script);
    assert_eq!(
        tool.default_command,
        vec![
            "bash".to_owned(),
            "@file:tools/bin/demo-script.sh".to_owned()
        ]
    );
}

#[test]
fn generated_tool_files_are_workspace_relative_and_expandable() {
    let root = unique_temp_dir("hc-cli-generated-files");
    let files = vec![super::NaturalLanguageToolFileDraft {
        path: "tools/bin/demo.sh".to_owned(),
        content: "#!/usr/bin/env bash\necho demo\n".to_owned(),
        executable: true,
    }];

    let written = write_generated_tool_files_under(&files, &root).expect("files should write");

    assert_eq!(written.len(), 1);
    assert!(written[0].ends_with("tools/bin/demo.sh"));
    assert_eq!(
        fs::read_to_string(&written[0]).expect("script should read"),
        "#!/usr/bin/env bash\necho demo\n"
    );
    let expanded = expand_default_command_token_in_root("@file:tools/bin/demo.sh", &root)
        .expect("token should expand");
    assert_eq!(PathBuf::from(expanded), written[0]);
}

#[test]
fn invalid_model_create_command_returns_error_without_panicking() {
    let result = try_execute_create_tool_command_from_response(
        r#"/create-tool skill，使用原生 --description "bad""#,
    );
    assert!(result.is_err());
}

#[test]
fn sanitizes_provider_tool_call_markup() {
    let sanitized = sanitize_model_response(
        r#"<think>secret</think>
我来应用技能。
$SKILL tool.frontend-red-theme
<minimax:tool_call>
<invoke name="frontend-red-theme">
<parameter name="command">应用红色系前端开发技能</parameter>
</invoke>
</minimax:tool_call>"#,
    );

    assert!(!sanitized.contains("<think>"));
    assert!(!sanitized.contains("$SKILL"));
    assert!(!sanitized.contains("tool_call"));
    assert!(sanitized.contains("我来应用技能。"));
}

#[test]
fn extracts_persistable_html_code_block() {
    let blocks = extract_code_blocks(
        r#"这里是页面：

```html
<!DOCTYPE html>
<html>
<body>
  <form><input type="email"><button>登录</button></form>
</body>
</html>
```
"#,
    );

    assert_eq!(blocks.len(), 1);
    assert_eq!(code_block_extension(&blocks[0]), Some("html"));
    assert!(looks_like_complete_artifact(&blocks[0], "html"));
}

#[test]
fn parses_skill_build_json_from_model_response() {
    let build = parse_tool_build_response(
        r#"{"action":"create_skill","message":null,"tool":null,"skill":{"id":"fe-red","name":"Frontend Red","description":"Red themed frontend skill.","instructions":"Build polished red themed frontend UI.","tool_id":null,"execution_kind":"builtin","default_command":[],"tool_refs":[],"tags":["frontend","red-theme"]}}"#,
    )
    .expect("json should parse");

    let skill = skill_from_natural_language_draft(build.skill.expect("skill should exist"))
        .expect("skill should be valid");
    assert_eq!(skill.id, "skill.fe-red");
    assert_eq!(skill.resolved_tool_id(), "tool.fe-red");
    assert_eq!(skill.default_command, Vec::<String>::new());
    assert!(skill.instructions.contains("red themed"));
    assert!(skill.tags.iter().any(|tag| tag == "skill"));
}

#[test]
fn extracts_create_tool_command_without_confirmation_tail() {
    let command = extract_create_tool_command(
        r#"好的：
`/create-tool fe-red "Frontend Engineer - Red Edition" --description "red frontend" --command "echo red" --tag frontend`

是否需要我帮你执行这条命令？"#,
    )
    .expect("command should be extracted");

    assert_eq!(
        command,
        r#"fe-red "Frontend Engineer - Red Edition" --description "red frontend" --command "echo red" --tag frontend"#
    );
}

#[test]
fn builds_tool_creation_from_command_fallback() {
    let build = build_from_create_tool_command(
        r#"fe-red "Frontend Engineer - Red Edition" --description "red frontend" --command "echo red" --tag frontend"#,
    )
    .expect("command fallback should build");
    let tool = build.tool.expect("tool draft should exist");

    assert_eq!(tool.id, "tool.fe-red");
    assert_eq!(tool.name, "Frontend Engineer - Red Edition");
    assert_eq!(tool.default_command, vec!["echo red".to_owned()]);
}

#[test]
fn builtin_local_file_tool_writes_and_reads_content() {
    let root = unique_temp_dir("hc-cli-file-tool");
    fs::create_dir_all(&root).expect("temp dir should create");
    let path = root.join("login.html");
    let write_tool = local_file_tool("tool.local-file.write", "hc.local-file.write");
    let write_plan = ToolExecutionPlan {
        tool_id: write_tool.id.clone(),
        suggested_command: vec![
            "hc.local-file.write".to_owned(),
            path.display().to_string(),
            "--content".to_owned(),
            "<content>".to_owned(),
        ],
        guidance: Vec::new(),
        validation_steps: Vec::new(),
        recovery_steps: Vec::new(),
    };
    let write_options = super::RunOptions {
        content: Some("<html>ok</html>".to_owned()),
        args: vec![path.display().to_string()],
        ..super::RunOptions::default()
    };

    let write_outcome =
        execute_builtin_tool(&write_tool, &write_plan, &write_options, "write login page")
            .expect("write should execute")
            .expect("builtin should handle write");

    assert!(write_outcome.success);
    assert_eq!(
        fs::read_to_string(&path).expect("written file should read"),
        "<html>ok</html>"
    );

    let read_tool = local_file_tool("tool.local-file.read", "hc.local-file.read");
    let read_plan = ToolExecutionPlan {
        tool_id: read_tool.id.clone(),
        suggested_command: vec!["hc.local-file.read".to_owned(), path.display().to_string()],
        guidance: Vec::new(),
        validation_steps: Vec::new(),
        recovery_steps: Vec::new(),
    };
    let read_options = super::RunOptions {
        args: vec![path.display().to_string()],
        ..super::RunOptions::default()
    };
    let read_outcome =
        execute_builtin_tool(&read_tool, &read_plan, &read_options, "read login page")
            .expect("read should execute")
            .expect("builtin should handle read");

    assert!(read_outcome.success);
    assert!(
        read_outcome
            .observations
            .iter()
            .any(|line| line.contains("<html>ok</html>"))
    );
}

#[test]
fn builtin_local_dir_tool_lists_entries() {
    let root = unique_temp_dir("hc-cli-dir-tool");
    fs::create_dir_all(root.join("nested")).expect("nested dir should create");
    fs::write(root.join("alpha.txt"), "alpha").expect("file should write");
    let tool = local_file_tool("tool.local-dir.list", "hc.local-dir.list");
    let plan = ToolExecutionPlan {
        tool_id: tool.id.clone(),
        suggested_command: vec!["hc.local-dir.list".to_owned(), root.display().to_string()],
        guidance: Vec::new(),
        validation_steps: Vec::new(),
        recovery_steps: Vec::new(),
    };
    let options = super::RunOptions {
        args: vec![root.display().to_string()],
        ..super::RunOptions::default()
    };

    let outcome = execute_builtin_tool(&tool, &plan, &options, "list directory")
        .expect("dir list should execute")
        .expect("builtin should handle dir list");

    assert!(outcome.success);
    assert!(outcome.observations.iter().any(|line| line == "entries: 2"));
    assert!(
        outcome
            .observations
            .iter()
            .any(|line| line.contains("entry: file alpha.txt"))
    );
    assert!(
        outcome
            .observations
            .iter()
            .any(|line| line.contains("entry: dir nested"))
    );
}

fn local_file_tool(id: &str, token: &str) -> ToolSpec {
    ToolSpec {
        id: id.to_owned(),
        name: id.to_owned(),
        description: "local file tool".to_owned(),
        execution_kind: ToolExecutionKind::Builtin,
        composition: ToolComposition::Atomic,
        stability: ToolStability::Stable,
        model_dependence: ModelDependence::Optional,
        default_command: vec![token.to_owned()],
        tags: vec!["local-file".to_owned()],
    }
}

fn unique_temp_dir(label: &str) -> PathBuf {
    let suffix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should work")
        .as_nanos();
    std::env::temp_dir().join(format!("{label}-{suffix}"))
}
