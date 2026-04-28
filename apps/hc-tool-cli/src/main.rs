use std::{
    collections::BTreeMap,
    env, fs,
    io::{self, Write},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use hc_capability::ModelDependence;
use hc_context::{
    load_tool_chat_prompt, load_tool_natural_language_builder_prompt, load_tool_router_prompt,
};
use hc_llm::{
    ChatMessage, GenerateRequest, MessageRole, ModelRef, ProviderRegistry, default_model_from_env,
    default_provider_from_env, default_registry_from_env, is_timeout_error,
};
use hc_skill::{SkillProfile, SkillRepository};
use hc_store::store::WorkspaceNamespace;
use hc_toolchain::{
    CommandToolExecutor, ToolCatalog, ToolComposition, ToolExecutionKind, ToolExecutionOutcome,
    ToolExecutor, ToolRepository, ToolSpec, ToolStability, build_default_tool_execution_plan,
    default_tool_catalog,
};
use rustyline::{DefaultEditor, error::ReadlineError};
use serde::Deserialize;

#[derive(Debug, Clone, Default)]
struct CommonOptions {
    json: bool,
}

#[derive(Debug, Clone, Default)]
struct RunOptions {
    json: bool,
    path: Option<PathBuf>,
    package: Option<String>,
    goal: Option<String>,
    content: Option<String>,
    args: Vec<String>,
}

#[derive(Debug, Clone)]
struct CreateOptions {
    id: String,
    name: String,
    description: String,
    execution_kind: ToolExecutionKind,
    command: Vec<String>,
    tags: Vec<String>,
    json: bool,
}

#[derive(Debug, Clone)]
struct ResolvedToolTarget {
    tool: ToolSpec,
    delegated_tool: Option<ToolSpec>,
    skill: Option<SkillProfile>,
}

#[derive(Debug, Clone)]
struct ToolSelection {
    selected: Option<ToolSpec>,
    candidates: Vec<ToolSelectionCandidate>,
}

#[derive(Debug, Clone)]
struct ToolSelectionCandidate {
    tool: ToolSpec,
    score: i32,
}

#[derive(Debug, Clone)]
struct CodeBlock {
    language: Option<String>,
    content: String,
}

trait ToolSelector {
    fn select(&self, input: &str, catalog: &ToolCatalog) -> Result<ToolSelection>;
}

#[derive(Debug, Clone, Copy)]
struct KeywordToolSelector {
    limit: usize,
}

impl Default for KeywordToolSelector {
    fn default() -> Self {
        Self { limit: 5 }
    }
}

impl ToolSelector for KeywordToolSelector {
    fn select(&self, input: &str, catalog: &ToolCatalog) -> Result<ToolSelection> {
        let mut candidates: Vec<ToolSelectionCandidate> = catalog
            .list()
            .into_iter()
            .map(|tool| ToolSelectionCandidate {
                tool: tool.clone(),
                score: score_tool_for_goal(tool, input),
            })
            .collect();
        candidates.sort_by(|left, right| {
            right
                .score
                .cmp(&left.score)
                .then_with(|| left.tool.id.cmp(&right.tool.id))
        });
        candidates.truncate(self.limit);
        let selected = candidates
            .first()
            .filter(|candidate| candidate.score > 0)
            .map(|candidate| candidate.tool.clone());
        Ok(ToolSelection {
            selected,
            candidates,
        })
    }
}

#[derive(Debug, Clone, Deserialize)]
struct NaturalLanguageToolBuild {
    action: String,
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    tool: Option<NaturalLanguageToolDraft>,
    #[serde(default)]
    skill: Option<NaturalLanguageSkillDraft>,
}

#[derive(Debug, Clone, Deserialize)]
struct NaturalLanguageToolRoute {
    action: String,
    #[serde(default)]
    tool_id: Option<String>,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    goal: Option<String>,
    #[serde(default)]
    message: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct NaturalLanguageToolDraft {
    id: String,
    name: String,
    description: String,
    execution_kind: Option<String>,
    default_command: Vec<String>,
    #[serde(default)]
    files: Vec<NaturalLanguageToolFileDraft>,
    tags: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct NaturalLanguageToolFileDraft {
    path: String,
    content: String,
    #[serde(default)]
    executable: bool,
}

#[derive(Debug, Clone, Deserialize)]
struct NaturalLanguageSkillDraft {
    id: String,
    name: String,
    description: String,
    instructions: String,
    tool_id: Option<String>,
    execution_kind: Option<String>,
    default_command: Vec<String>,
    tool_refs: Vec<String>,
    tags: Vec<String>,
}

fn main() -> Result<()> {
    load_local_env_file()?;
    let args: Vec<String> = env::args().skip(1).collect();
    match args.as_slice() {
        [] => handle_chat(&[]),
        [cmd] if is_help(cmd) => {
            print_help();
            Ok(())
        }
        [cmd, rest @ ..] if cmd == "chat" => handle_chat(rest),
        [cmd, rest @ ..] if cmd == "create" => handle_create(rest),
        [cmd, rest @ ..] if cmd == "list" => handle_list(rest),
        [cmd, rest @ ..] if cmd == "show" => handle_show(rest),
        [cmd, rest @ ..] if cmd == "plan" => handle_plan(rest),
        [cmd, rest @ ..] if cmd == "run" => handle_run(rest),
        [other, ..] => bail!("unknown command: {other}"),
    }
}

fn handle_chat(args: &[String]) -> Result<()> {
    let mut provider = default_provider();
    let mut model = default_model();
    let mut system_message = env::var("HC_LLM_SYSTEM").ok();

    let mut index = 0usize;
    while index < args.len() {
        match args[index].as_str() {
            "--provider" => {
                provider = args
                    .get(index + 1)
                    .cloned()
                    .context("missing value for --provider")?;
                index += 2;
            }
            "--model" => {
                model = args
                    .get(index + 1)
                    .cloned()
                    .context("missing value for --model")?;
                index += 2;
            }
            "--system" => {
                system_message = Some(
                    args.get(index + 1)
                        .cloned()
                        .context("missing value for --system")?,
                );
                index += 2;
            }
            other => bail!("unknown chat option: {other}"),
        }
    }

    let registry = default_registry();
    let catalog = load_cli_tool_catalog()?;
    let tool_prompt = render_tool_chat_system_prompt(&catalog, system_message.as_deref())?;

    println!("hc-tool chat");
    println!("provider={provider} model={model}");
    println!("Type /help for commands, /quit to exit.");

    let mut editor = DefaultEditor::new().context("failed to initialize line editor")?;
    let mut history = vec![ChatMessage::new(MessageRole::System, tool_prompt)];
    loop {
        let Some(input) = prompt_raw(&mut editor)? else {
            break;
        };
        let trimmed = input.trim();
        if trimmed.is_empty() {
            continue;
        }

        match trimmed {
            "/quit" | "/exit" => break,
            "/help" => {
                println!("/help");
                println!("/clear");
                println!("/tools");
                println!("/plan <goal>");
                println!(
                    "/create-tool <id> <name> --description <text> --command <token> [--command <token>] [--tag <tag>]"
                );
                println!("/quit");
                continue;
            }
            "/clear" => {
                let catalog = load_cli_tool_catalog()?;
                history.clear();
                history.push(ChatMessage::new(
                    MessageRole::System,
                    render_tool_chat_system_prompt(&catalog, system_message.as_deref())?,
                ));
                println!("history cleared");
                continue;
            }
            "/tools" => {
                let catalog = load_cli_tool_catalog()?;
                for tool in catalog.list() {
                    println!("{} | {} | {}", tool.id, tool.name, tool.description);
                }
                continue;
            }
            _ if trimmed.starts_with("/plan ") => {
                let catalog = load_cli_tool_catalog()?;
                let goal = trimmed
                    .strip_prefix("/plan ")
                    .map(str::trim)
                    .unwrap_or_default();
                if goal.is_empty() {
                    println!("usage: /plan <goal>");
                    continue;
                }
                let (tool, _) = auto_select_tool(&catalog, goal)?;
                let plan = build_default_tool_execution_plan(&tool, goal)?;
                println!("tool> {}", plan.tool_id);
                println!("command> {}", plan.suggested_command.join(" "));
                print_lines("guidance", &plan.guidance);
                continue;
            }
            _ if trimmed.starts_with("/create-tool ") => {
                match handle_create_from_chat(trimmed.strip_prefix("/create-tool ").unwrap_or("")) {
                    Ok(path) => {
                        println!("created> {}", path.display());
                        let catalog = load_cli_tool_catalog()?;
                        history.clear();
                        history.push(ChatMessage::new(
                            MessageRole::System,
                            render_tool_chat_system_prompt(&catalog, system_message.as_deref())?,
                        ));
                    }
                    Err(error) => println!("error> {error}"),
                }
                continue;
            }
            _ => {}
        }

        let catalog = load_cli_tool_catalog()?;
        let selector = KeywordToolSelector::default();
        let selection_input = selection_input_from_history(&history, trimmed);
        let mut selection = selector.select(&selection_input, &catalog)?;
        let mut tool_execution_context = None;
        match route_tool_turn(
            &registry,
            &provider,
            &model,
            trimmed,
            &selection,
            system_message.as_deref(),
        ) {
            Ok(route) if route.action == "create_tool" || route.action == "create_skill" => {
                match handle_natural_language_tool_create(
                    &registry,
                    &provider,
                    &model,
                    trimmed,
                    system_message.as_deref(),
                ) {
                    Ok(true) => {
                        let catalog = load_cli_tool_catalog()?;
                        history.clear();
                        history.push(ChatMessage::new(
                            MessageRole::System,
                            render_tool_chat_system_prompt(&catalog, system_message.as_deref())?,
                        ));
                        continue;
                    }
                    Ok(false) => {}
                    Err(error) => {
                        println!("warning> tool builder skipped: {error}");
                    }
                }
            }
            Ok(route) if route.action == "run_tool" => {
                let context = execute_routed_tool(&route)?;
                apply_tool_route(&mut selection, &catalog, route)?;
                tool_execution_context = Some(context);
            }
            Ok(route) => {
                apply_tool_route(&mut selection, &catalog, route)?;
            }
            Err(error) => {
                println!("{}", render_router_warning(&error));
            }
        }
        let request_history = build_chat_request_history(
            &history,
            merge_optional_contexts([
                render_tool_selection_context(&selection),
                tool_execution_context,
            ]),
            trimmed,
        );
        let request = GenerateRequest::new(
            ModelRef::new(provider.clone(), model.clone()),
            request_history,
        );
        print!("assistant> ");
        io::stdout().flush().context("failed to flush stdout")?;
        match registry.generate(&request) {
            Ok(response) => {
                let display_content = sanitize_model_response(&response.message.content);
                match try_execute_create_tool_command_from_response(&display_content) {
                    Ok(Some(path)) => {
                        println!("已创建> {}", path.display());
                        let catalog = load_cli_tool_catalog()?;
                        history.clear();
                        history.push(ChatMessage::new(
                            MessageRole::System,
                            render_tool_chat_system_prompt(&catalog, system_message.as_deref())?,
                        ));
                    }
                    Ok(None) => {
                        if display_content.trim().is_empty() {
                            println!(
                                "warning> model emitted a provider tool-call marker instead of normal text; ignored it. Please retry."
                            );
                        } else {
                            println!("{display_content}");
                            for path in persist_response_artifacts(trimmed, &display_content)? {
                                println!("saved> {}", path.display());
                            }
                        }
                        history.push(ChatMessage::new(MessageRole::User, trimmed.to_owned()));
                        history.push(ChatMessage::new(MessageRole::Assistant, display_content));
                    }
                    Err(error) => {
                        println!("{display_content}");
                        println!("warning> ignored invalid create command from model: {error}");
                        history.push(ChatMessage::new(MessageRole::User, trimmed.to_owned()));
                        history.push(ChatMessage::new(MessageRole::Assistant, display_content));
                    }
                }
            }
            Err(error) => {
                println!("{}", render_chat_error(&error));
            }
        }
    }

    Ok(())
}

fn handle_list(args: &[String]) -> Result<()> {
    let options = parse_common_options(args)?;
    let catalog = load_cli_tool_catalog()?;
    let tools: Vec<&ToolSpec> = catalog.list();

    if options.json {
        println!("{}", serde_json::to_string_pretty(&tools)?);
        return Ok(());
    }

    for tool in tools {
        println!(
            "{} | {} | {:?} | {:?}",
            tool.id, tool.name, tool.execution_kind, tool.stability
        );
    }
    Ok(())
}

fn handle_show(args: &[String]) -> Result<()> {
    let (selector, options) = parse_selector_and_common_options(args, "show")?;
    let catalog = load_cli_tool_catalog()?;
    let tool = resolve_tool(&catalog, &selector)?;

    if options.json {
        println!("{}", serde_json::to_string_pretty(&tool)?);
        return Ok(());
    }

    println!("id> {}", tool.id);
    println!("name> {}", tool.name);
    println!("description> {}", tool.description);
    println!("kind> {:?}", tool.execution_kind);
    println!("composition> {:?}", tool.composition);
    println!("stability> {:?}", tool.stability);
    println!("command> {}", tool.default_command.join(" "));
    if !tool.tags.is_empty() {
        println!("tags> {}", tool.tags.join(", "));
    }
    Ok(())
}

fn handle_create(args: &[String]) -> Result<()> {
    let options = parse_create_options(args)?;
    let tool = tool_from_create_options(&options)?;
    let path = tool_repository().write_tool(&tool)?;

    if options.json {
        let payload = serde_json::json!({
            "tool": tool,
            "path": path,
        });
        println!("{}", serde_json::to_string_pretty(&payload)?);
        return Ok(());
    }

    println!("created> {}", path.display());
    println!("tool> {}", tool.id);
    println!("command> {}", tool.default_command.join(" "));
    Ok(())
}

fn handle_create_from_chat(input: &str) -> Result<PathBuf> {
    let args = split_chat_command(input);
    let options = parse_create_options(&args)?;
    let tool = tool_from_create_options(&options)?;
    tool_repository().write_tool(&tool)
}

fn handle_natural_language_tool_create(
    registry: &ProviderRegistry,
    provider: &str,
    model: &str,
    input: &str,
    user_system: Option<&str>,
) -> Result<bool> {
    let catalog = load_cli_tool_catalog()?;
    let build = synthesize_tool_build_from_natural_language(
        registry,
        provider,
        model,
        input,
        &catalog,
        user_system,
    )?;

    match build.action.as_str() {
        "ignore" => Ok(false),
        "ask_clarification" => {
            println!(
                "assistant> {}",
                build.message.unwrap_or_else(|| {
                    "还缺少工具 id、用途或默认命令，请补充一下。".to_owned()
                })
            );
            Ok(true)
        }
        "create_tool" => {
            let draft = build.tool.context("tool creation response missed tool")?;
            let generated_files = draft.files.clone();
            let tool = tool_from_natural_language_draft(draft)?;
            if catalog.contains(&tool.id) {
                bail!(
                    "tool {} already exists. Use /create-tool for an explicit overwrite.",
                    tool.id
                );
            }
            let file_paths = write_generated_tool_files(&generated_files)?;
            let path = tool_repository().write_tool(&tool)?;
            println!(
                "assistant> 已创建工具 {} ({})，保存到 {}",
                tool.id,
                tool.name,
                path.display()
            );
            for file_path in file_paths {
                println!("file> {}", file_path.display());
            }
            println!("command> {}", tool.default_command.join(" "));
            Ok(true)
        }
        "create_skill" => {
            let draft = build
                .skill
                .context("skill creation response missed skill")?;
            let skill = skill_from_natural_language_draft(draft)?;
            let path = SkillRepository::relative_path_for(&skill);
            if skill_repository().read_profile(&path).is_ok() {
                println!(
                    "assistant> skill {} ({}) 已存在：{}",
                    skill.id,
                    skill.name,
                    path.display()
                );
                return Ok(true);
            }
            let path = skill_repository().write_profile(&skill)?;
            println!(
                "assistant> 已创建 skill {} ({})，保存到 {}",
                skill.id,
                skill.name,
                path.display()
            );
            if let Some(tool_id) = skill.delegated_tool_id() {
                println!("delegates> {tool_id}");
            }
            Ok(true)
        }
        _ => Ok(false),
    }
}

fn handle_plan(args: &[String]) -> Result<()> {
    if args.is_empty() {
        bail!("usage: hc-tool-cli plan <auto|rg|cargo-test|tool-id> <goal...> [--json]");
    }

    let mut json = false;
    let mut positional = Vec::new();
    for arg in args {
        match arg.as_str() {
            "--json" => json = true,
            other => positional.push(other.to_owned()),
        }
    }

    let selector = positional
        .first()
        .cloned()
        .context("missing tool selector for plan")?;
    let goal = positional
        .get(1..)
        .filter(|parts| !parts.is_empty())
        .map(|parts| parts.join(" "))
        .context("missing goal for plan")?;

    let catalog = load_cli_tool_catalog()?;
    let (resolved, selected_tool, candidates) = if selector == "auto" {
        let (selected_tool, candidates) = auto_select_tool(&catalog, &goal)?;
        (
            resolve_tool_selector(&selected_tool.id)?,
            selected_tool,
            candidates,
        )
    } else {
        let resolved = resolve_tool_selector(&selector)?;
        (resolved.clone(), resolved.tool, Vec::new())
    };
    let planning_tool = resolved.delegated_tool.as_ref().unwrap_or(&resolved.tool);
    let mut plan = build_default_tool_execution_plan(planning_tool, &goal)?;
    plan.tool_id = resolved.tool.id.clone();
    if let Some(skill) = &resolved.skill
        && !skill.instructions.trim().is_empty()
    {
        plan.guidance.insert(0, skill.instructions.clone());
    }

    if json {
        let payload = serde_json::json!({
            "tool": resolved.tool,
            "delegated_tool": resolved.delegated_tool,
            "skill": resolved.skill,
            "plan": plan,
            "candidates": candidates,
        });
        println!("{}", serde_json::to_string_pretty(&payload)?);
        return Ok(());
    }

    if selector == "auto" {
        println!("selected> {}", selected_tool.id);
    }
    println!("tool> {}", plan.tool_id);
    println!("command> {}", plan.suggested_command.join(" "));
    print_lines("guidance", &plan.guidance);
    print_lines("validation", &plan.validation_steps);
    print_lines("recovery", &plan.recovery_steps);
    Ok(())
}

fn handle_run(args: &[String]) -> Result<()> {
    if args.is_empty() {
        bail!("usage: hc-tool-cli run <rg|cargo-test|tool-id> [args...] [--json]");
    }

    let selector = args[0].clone();
    let options = parse_run_options(&args[1..])?;
    let (plan, outcome) = execute_tool_by_selector(&selector, &options)?;

    if options.json {
        let payload = serde_json::json!({
            "plan": plan,
            "outcome": outcome,
        });
        println!("{}", serde_json::to_string_pretty(&payload)?);
        return Ok(());
    }

    println!("tool> {}", outcome.tool_id);
    println!("success> {}", outcome.success);
    println!("summary> {}", outcome.summary);
    println!("command> {}", outcome.command.join(" "));
    print_lines("output", &outcome.observations);
    Ok(())
}

fn execute_tool_by_selector(
    selector: &str,
    options: &RunOptions,
) -> Result<(hc_toolchain::ToolExecutionPlan, ToolExecutionOutcome)> {
    let resolved = resolve_tool_selector(selector)?;
    execute_resolved_tool(resolved, options)
}

fn execute_resolved_tool(
    resolved: ResolvedToolTarget,
    options: &RunOptions,
) -> Result<(hc_toolchain::ToolExecutionPlan, ToolExecutionOutcome)> {
    let tool = resolved.tool.clone();
    let delegated_tool = resolved
        .delegated_tool
        .clone()
        .unwrap_or_else(|| tool.clone());
    let goal = options
        .goal
        .clone()
        .unwrap_or_else(|| default_run_goal(&tool, &options.args));
    let mut plan = build_default_tool_execution_plan(&delegated_tool, &goal)?;
    plan.tool_id = tool.id.clone();
    if let Some(skill) = &resolved.skill
        && !skill.instructions.trim().is_empty()
    {
        plan.guidance.insert(0, skill.instructions.clone());
    }
    plan.suggested_command = runnable_command(&delegated_tool, &options)?;

    let delegated_plan = hc_toolchain::ToolExecutionPlan {
        tool_id: delegated_tool.id.clone(),
        suggested_command: plan.suggested_command.clone(),
        guidance: plan.guidance.clone(),
        validation_steps: plan.validation_steps.clone(),
        recovery_steps: plan.recovery_steps.clone(),
    };
    let atomic_outcome =
        match execute_builtin_tool(&delegated_tool, &delegated_plan, &options, &goal)? {
            Some(outcome) => outcome,
            None => {
                let executor = match &options.path {
                    Some(path) => CommandToolExecutor::new().with_working_dir(path),
                    None => CommandToolExecutor::new(),
                };
                executor.execute(&delegated_plan, &goal)?
            }
        };
    let outcome = if delegated_tool.id != tool.id {
        atomic_outcome.wrapped_by(tool.id.clone())
    } else {
        atomic_outcome
    };

    Ok((plan, outcome))
}

fn parse_common_options(args: &[String]) -> Result<CommonOptions> {
    let mut options = CommonOptions::default();
    for arg in args {
        match arg.as_str() {
            "--json" => options.json = true,
            other => bail!("unexpected argument: {other}"),
        }
    }
    Ok(options)
}

fn parse_selector_and_common_options(
    args: &[String],
    command_name: &str,
) -> Result<(String, CommonOptions)> {
    let mut selector = None;
    let mut options = CommonOptions::default();
    for arg in args {
        match arg.as_str() {
            "--json" => options.json = true,
            other if selector.is_none() => selector = Some(other.to_owned()),
            other => bail!("unexpected argument for {command_name}: {other}"),
        }
    }
    let selector = selector.with_context(|| format!("missing tool selector for {command_name}"))?;
    Ok((selector, options))
}

fn parse_run_options(args: &[String]) -> Result<RunOptions> {
    let mut options = RunOptions::default();
    let mut index = 0usize;
    while index < args.len() {
        match args[index].as_str() {
            "--json" => {
                options.json = true;
                index += 1;
            }
            "--path" => {
                options.path = Some(PathBuf::from(
                    args.get(index + 1)
                        .context("missing value for --path")?
                        .as_str(),
                ));
                index += 2;
            }
            "--package" | "-p" => {
                options.package = Some(args.get(index + 1).cloned().context("missing package")?);
                index += 2;
            }
            "--goal" => {
                options.goal = Some(
                    args.get(index + 1)
                        .cloned()
                        .context("missing value for --goal")?,
                );
                index += 2;
            }
            "--content" => {
                options.content = Some(
                    args.get(index + 1)
                        .cloned()
                        .context("missing value for --content")?,
                );
                index += 2;
            }
            value => {
                options.args.push(value.to_owned());
                index += 1;
            }
        }
    }
    Ok(options)
}

fn execute_routed_tool(route: &NaturalLanguageToolRoute) -> Result<String> {
    let tool_id = route
        .tool_id
        .as_deref()
        .filter(|tool_id| !tool_id.trim().is_empty())
        .context("tool router selected run_tool without tool_id")?;
    let options = RunOptions {
        goal: route.goal.clone(),
        args: route.args.clone(),
        ..RunOptions::default()
    };
    let (plan, outcome) = execute_tool_by_selector(tool_id, &options)?;
    Ok(render_tool_execution_context(&plan, &outcome))
}

fn execute_builtin_tool(
    tool: &ToolSpec,
    plan: &hc_toolchain::ToolExecutionPlan,
    options: &RunOptions,
    goal: &str,
) -> Result<Option<ToolExecutionOutcome>> {
    if tool.execution_kind != ToolExecutionKind::Builtin {
        return Ok(None);
    }

    let Some(token) = tool.default_command.first() else {
        return Ok(None);
    };

    match token.as_str() {
        "hc.local-file.read" => execute_local_file_read(plan, options, goal).map(Some),
        "hc.local-file.write" => execute_local_file_write(plan, options, goal).map(Some),
        "hc.local-dir.list" => execute_local_dir_list(plan, options, goal).map(Some),
        _ => bail!("unsupported builtin tool command: {token}"),
    }
}

fn execute_local_file_read(
    plan: &hc_toolchain::ToolExecutionPlan,
    options: &RunOptions,
    goal: &str,
) -> Result<ToolExecutionOutcome> {
    let path_arg = options
        .args
        .first()
        .context("missing file path for local file read")?;
    let path = resolve_run_file_path(options.path.as_deref(), path_arg)?;
    let content = fs::read_to_string(&path)
        .with_context(|| format!("failed to read local file: {}", path.display()))?;

    let mut observations = vec![
        format!("path: {}", path.display()),
        format!("bytes: {}", content.len()),
    ];
    for line in content.lines().take(120) {
        observations.push(format!("content: {line}"));
    }
    if content.lines().count() > 120 {
        observations.push("content: ... truncated".to_owned());
    }

    Ok(ToolExecutionOutcome {
        tool_id: plan.tool_id.clone(),
        parent_tool_id: None,
        invoked_tool_ids: Vec::new(),
        goal: goal.to_owned(),
        command: plan.suggested_command.clone(),
        success: true,
        summary: format!("read {} bytes from {}", content.len(), path.display()),
        observations,
    })
}

fn execute_local_file_write(
    plan: &hc_toolchain::ToolExecutionPlan,
    options: &RunOptions,
    goal: &str,
) -> Result<ToolExecutionOutcome> {
    let path_arg = options
        .args
        .first()
        .context("missing file path for local file write")?;
    let content = options
        .content
        .clone()
        .or_else(|| {
            if options.args.len() > 1 {
                Some(
                    options
                        .args
                        .iter()
                        .skip(1)
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(" "),
                )
            } else {
                None
            }
        })
        .context("missing content for local file write; use --content <text>")?;
    let path = resolve_run_file_path(options.path.as_deref(), path_arg)?;
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create parent directory: {}", parent.display()))?;
    }
    fs::write(&path, content.as_bytes())
        .with_context(|| format!("failed to write local file: {}", path.display()))?;

    Ok(ToolExecutionOutcome {
        tool_id: plan.tool_id.clone(),
        parent_tool_id: None,
        invoked_tool_ids: Vec::new(),
        goal: goal.to_owned(),
        command: plan.suggested_command.clone(),
        success: true,
        summary: format!("wrote {} bytes to {}", content.len(), path.display()),
        observations: vec![
            format!("path: {}", path.display()),
            format!("bytes: {}", content.len()),
        ],
    })
}

fn execute_local_dir_list(
    plan: &hc_toolchain::ToolExecutionPlan,
    options: &RunOptions,
    goal: &str,
) -> Result<ToolExecutionOutcome> {
    let path_arg = options.args.first().map(String::as_str).unwrap_or(".");
    let path = resolve_run_file_path(options.path.as_deref(), path_arg)?;
    let mut entries = fs::read_dir(&path)
        .with_context(|| format!("failed to list local directory: {}", path.display()))?
        .collect::<std::result::Result<Vec<_>, _>>()
        .with_context(|| format!("failed to read local directory entry: {}", path.display()))?;
    entries.sort_by_key(|entry| entry.file_name());

    let mut observations = vec![format!("path: {}", path.display())];
    observations.push(format!("entries: {}", entries.len()));
    for entry in entries.iter().take(200) {
        let file_type = entry
            .file_type()
            .with_context(|| format!("failed to read file type: {}", entry.path().display()))?;
        let kind = if file_type.is_dir() {
            "dir"
        } else if file_type.is_file() {
            "file"
        } else if file_type.is_symlink() {
            "symlink"
        } else {
            "other"
        };
        observations.push(format!(
            "entry: {kind} {}",
            entry.file_name().to_string_lossy()
        ));
    }
    if entries.len() > 200 {
        observations.push("entry: ... truncated".to_owned());
    }

    Ok(ToolExecutionOutcome {
        tool_id: plan.tool_id.clone(),
        parent_tool_id: None,
        invoked_tool_ids: Vec::new(),
        goal: goal.to_owned(),
        command: plan.suggested_command.clone(),
        success: true,
        summary: format!("listed {} entries in {}", entries.len(), path.display()),
        observations,
    })
}

fn resolve_run_file_path(base: Option<&Path>, path_arg: &str) -> Result<PathBuf> {
    let path = PathBuf::from(path_arg);
    if path.is_absolute() {
        return Ok(path);
    }
    let base = match base {
        Some(base) => base.to_path_buf(),
        None => env::current_dir().context("failed to resolve current directory")?,
    };
    Ok(base.join(path))
}

fn parse_create_options(args: &[String]) -> Result<CreateOptions> {
    if args.len() < 2 {
        bail!(
            "usage: hc-tool-cli create <tool-id> <name> --description <text> --command <token> [--command <token>] [--kind <cli|builtin|script|workflow|service>] [--tag <tag>] [--json]"
        );
    }

    let id = normalize_tool_id(&args[0]);
    let name = args[1].clone();
    let mut description: Option<String> = None;
    let mut execution_kind = ToolExecutionKind::Cli;
    let mut command = Vec::new();
    let mut tags = Vec::new();
    let mut json = false;

    let mut index = 2usize;
    while index < args.len() {
        match args[index].as_str() {
            "--description" => {
                description = Some(
                    args.get(index + 1)
                        .cloned()
                        .context("missing value for --description")?,
                );
                index += 2;
            }
            "--kind" => {
                execution_kind = parse_tool_execution_kind(
                    args.get(index + 1).context("missing value for --kind")?,
                )?;
                index += 2;
            }
            "--command" => {
                command.push(
                    args.get(index + 1)
                        .cloned()
                        .context("missing value for --command")?,
                );
                index += 2;
            }
            "--tag" => {
                tags.push(
                    args.get(index + 1)
                        .cloned()
                        .context("missing value for --tag")?,
                );
                index += 2;
            }
            "--json" => {
                json = true;
                index += 1;
            }
            other => bail!("unexpected argument for create: {other}"),
        }
    }

    let description = description.context("missing --description for create")?;
    if command.is_empty()
        && matches!(
            execution_kind,
            ToolExecutionKind::Cli | ToolExecutionKind::Script
        )
    {
        bail!("missing --command for executable tool");
    }

    Ok(CreateOptions {
        id,
        name,
        description,
        execution_kind,
        command,
        tags,
        json,
    })
}

fn tool_from_create_options(options: &CreateOptions) -> Result<ToolSpec> {
    let mut tags = options.tags.clone();
    if !tags.iter().any(|tag| tag == "tool") {
        tags.push("tool".to_owned());
    }

    let tool = ToolSpec {
        id: options.id.clone(),
        name: options.name.clone(),
        description: options.description.clone(),
        execution_kind: options.execution_kind.clone(),
        composition: ToolComposition::Atomic,
        stability: ToolStability::Managed,
        model_dependence: ModelDependence::Optional,
        default_command: options.command.clone(),
        tags,
    };
    hc_toolchain::validate_tool_spec(&tool)?;
    Ok(tool)
}

fn tool_from_natural_language_draft(draft: NaturalLanguageToolDraft) -> Result<ToolSpec> {
    let execution_kind = match draft.execution_kind.as_deref() {
        Some(value) => parse_tool_execution_kind(value)?,
        None => ToolExecutionKind::Cli,
    };
    let options = CreateOptions {
        id: normalize_tool_id(draft.id.trim()),
        name: draft.name.trim().to_owned(),
        description: draft.description.trim().to_owned(),
        execution_kind,
        command: draft.default_command,
        tags: draft.tags,
        json: false,
    };
    tool_from_create_options(&options)
}

fn skill_from_natural_language_draft(draft: NaturalLanguageSkillDraft) -> Result<SkillProfile> {
    let execution_kind = match draft.execution_kind.as_deref() {
        Some(value) => parse_tool_execution_kind(value)?,
        None => ToolExecutionKind::Builtin,
    };
    let mut profile = SkillProfile::new(normalize_skill_id(draft.id.trim()), draft.name.trim())
        .with_namespace(runtime_namespace())
        .with_description(draft.description.trim())
        .with_instructions(draft.instructions.trim())
        .with_execution_kind(execution_kind)
        .with_model_dependence(ModelDependence::Optional)
        .with_default_command(draft.default_command);
    if let Some(tool_id) = draft.tool_id.filter(|value| !value.trim().is_empty()) {
        profile = profile.with_tool_id(normalize_tool_id(tool_id.trim()));
    }
    for tool_ref in draft.tool_refs {
        if !tool_ref.trim().is_empty() {
            profile = profile.with_tool_ref(normalize_tool_id(tool_ref.trim()));
        }
    }
    for tag in draft.tags {
        if !tag.trim().is_empty() {
            profile = profile.with_tag(tag);
        }
    }
    if !profile.tags.iter().any(|tag| tag == "skill") {
        profile = profile.with_tag("skill");
    }
    Ok(profile)
}

fn write_generated_tool_files(files: &[NaturalLanguageToolFileDraft]) -> Result<Vec<PathBuf>> {
    write_generated_tool_files_under(files, &workspace_namespace_root())
}

fn write_generated_tool_files_under(
    files: &[NaturalLanguageToolFileDraft],
    root: &Path,
) -> Result<Vec<PathBuf>> {
    let mut written = Vec::new();
    for file in files {
        let path = resolve_workspace_relative_file_under(&file.path, root)?;
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create directory: {}", parent.display()))?;
        }
        fs::write(&path, file.content.as_bytes())
            .with_context(|| format!("failed to write generated tool file: {}", path.display()))?;
        set_executable_if_requested(&path, file.executable)?;
        written.push(path);
    }
    Ok(written)
}

fn resolve_workspace_relative_file_under(path: &str, root: &Path) -> Result<PathBuf> {
    let relative = safe_relative_path(path)?;
    Ok(root.join(relative))
}

fn safe_relative_path(path: &str) -> Result<PathBuf> {
    let path = PathBuf::from(path.trim());
    if path.as_os_str().is_empty() {
        bail!("generated file path cannot be empty");
    }
    if path.is_absolute() {
        bail!("generated file path must be relative to the user workspace");
    }
    if path
        .components()
        .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        bail!("generated file path cannot contain parent directory components");
    }
    Ok(path)
}

fn set_executable_if_requested(path: &Path, executable: bool) -> Result<()> {
    if !executable {
        return Ok(());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(path)
            .with_context(|| format!("failed to read permissions: {}", path.display()))?
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions)
            .with_context(|| format!("failed to set executable bit: {}", path.display()))?;
    }
    Ok(())
}

fn parse_tool_execution_kind(value: &str) -> Result<ToolExecutionKind> {
    match value.trim().to_ascii_lowercase().as_str() {
        "cli" => Ok(ToolExecutionKind::Cli),
        "builtin" => Ok(ToolExecutionKind::Builtin),
        "script" => Ok(ToolExecutionKind::Script),
        "workflow" => Ok(ToolExecutionKind::Workflow),
        "service" => Ok(ToolExecutionKind::Service),
        other => bail!(
            "unsupported tool execution kind: {other}. supported kinds: cli, builtin, script, workflow, service"
        ),
    }
}

fn normalize_tool_id(value: &str) -> String {
    if value.starts_with("tool.") {
        value.to_owned()
    } else {
        format!("tool.{value}")
    }
}

fn normalize_skill_id(value: &str) -> String {
    if value.starts_with("skill.") {
        value.to_owned()
    } else if let Some(rest) = value.strip_prefix("tool.") {
        format!("skill.{rest}")
    } else {
        format!("skill.{value}")
    }
}

fn resolve_tool(catalog: &ToolCatalog, selector: &str) -> Result<ToolSpec> {
    let normalized = match selector {
        "rg" => "tool.rg",
        "cargo-test" => "tool.cargo-test",
        other => other,
    };
    catalog
        .get(normalized)
        .cloned()
        .with_context(|| format!("unknown tool selector: {selector}"))
}

fn resolve_tool_selector(selector: &str) -> Result<ResolvedToolTarget> {
    let normalized = normalize_tool_selector(selector);

    if normalized.starts_with("skill.") {
        let skill = skill_repository()
            .read_profile(format!("skills/{normalized}.md"))
            .with_context(|| format!("failed to load skill {normalized}"))?;
        let delegated_tool = match skill.delegated_tool_id() {
            Some(tool_id) => load_cli_tool_catalog()?
                .get(tool_id)
                .cloned()
                .with_context(|| format!("skill {normalized} references unknown tool {tool_id}"))?,
            None => skill.to_tool_spec(),
        };
        return Ok(ResolvedToolTarget {
            tool: skill.to_tool_spec(),
            delegated_tool: Some(delegated_tool),
            skill: Some(skill),
        });
    }

    let catalog = load_cli_tool_catalog()?;
    if let Some(tool) = catalog.get(&normalized) {
        if tool.composition == ToolComposition::Composite {
            if let Some(skill) = skill_repository()
                .list_profiles()?
                .into_iter()
                .find(|skill| skill.resolved_tool_id() == tool.id)
            {
                let delegated_tool = match skill.delegated_tool_id() {
                    Some(tool_id) => load_cli_tool_catalog()?
                        .get(tool_id)
                        .cloned()
                        .with_context(|| {
                            format!("skill {} references unknown tool {tool_id}", skill.id)
                        })?,
                    None => skill.to_tool_spec(),
                };
                return Ok(ResolvedToolTarget {
                    tool: tool.clone(),
                    delegated_tool: Some(delegated_tool),
                    skill: Some(skill),
                });
            }
        }

        return Ok(ResolvedToolTarget {
            tool: tool.clone(),
            delegated_tool: None,
            skill: None,
        });
    }

    bail!("unsupported tool or skill selector: {selector}")
}

fn normalize_tool_selector(selector: &str) -> String {
    match selector {
        "rg" => "tool.rg".to_owned(),
        "cargo-test" => "tool.cargo-test".to_owned(),
        other => other.to_owned(),
    }
}

fn auto_select_tool(
    catalog: &ToolCatalog,
    goal: &str,
) -> Result<(ToolSpec, Vec<serde_json::Value>)> {
    let mut scored: Vec<(ToolSpec, i32)> = catalog
        .list()
        .into_iter()
        .map(|tool| (tool.clone(), score_tool_for_goal(tool, goal)))
        .collect();
    scored.sort_by(|left, right| {
        right
            .1
            .cmp(&left.1)
            .then_with(|| left.0.id.cmp(&right.0.id))
    });

    let selected = scored
        .first()
        .map(|(tool, _)| tool.clone())
        .context("no tools are registered")?;
    let candidates = scored
        .into_iter()
        .map(|(tool, score)| {
            serde_json::json!({
                "tool_id": tool.id,
                "name": tool.name,
                "score": score,
            })
        })
        .collect();
    Ok((selected, candidates))
}

fn score_tool_for_goal(tool: &ToolSpec, goal: &str) -> i32 {
    let mut score = 0;
    for token in goal_match_terms(goal) {
        if token.is_empty() {
            continue;
        }
        let token_lowered = token.to_ascii_lowercase();
        if tool.id.to_ascii_lowercase().contains(&token_lowered) {
            score += 4;
        }
        if tool.name.to_ascii_lowercase().contains(&token_lowered) || tool.name.contains(&token) {
            score += 3;
        }
        if tool
            .description
            .to_ascii_lowercase()
            .contains(&token_lowered)
            || tool.description.contains(&token)
        {
            score += 2;
        }
        if tool
            .tags
            .iter()
            .any(|tag| tag.to_ascii_lowercase().contains(&token_lowered) || tag.contains(&token))
        {
            score += 2;
        }
    }
    score
}

fn goal_match_terms(goal: &str) -> Vec<String> {
    let mut terms = Vec::new();
    let lowered = goal.to_ascii_lowercase();
    for token in lowered.split(|ch: char| !ch.is_alphanumeric()) {
        if !token.is_empty() {
            terms.push(token.to_owned());
        }
    }

    let cjk_runs = goal
        .split(|ch: char| ch.is_ascii() || ch.is_whitespace() || ch.is_ascii_punctuation())
        .filter(|part| !part.is_empty());
    for run in cjk_runs {
        terms.push(run.to_owned());
        let chars: Vec<char> = run.chars().collect();
        for window in chars.windows(2) {
            terms.push(window.iter().collect());
        }
    }
    terms.sort();
    terms.dedup();
    terms
}

fn runnable_command(tool: &ToolSpec, options: &RunOptions) -> Result<Vec<String>> {
    match tool.id.as_str() {
        "tool.rg" => {
            let pattern = options
                .args
                .first()
                .context("missing search pattern for rg")?;
            let mut command = vec!["rg".to_owned(), "-n".to_owned(), pattern.clone()];
            command.extend(options.args.iter().skip(1).cloned());
            Ok(command)
        }
        "tool.cargo-test" => {
            let mut command = vec!["cargo".to_owned(), "test".to_owned()];
            if let Some(package) = &options.package {
                command.extend(["-p".to_owned(), package.clone()]);
            }
            command.extend(options.args.iter().cloned());
            Ok(command)
        }
        _ => {
            let mut command = tool
                .default_command
                .iter()
                .map(|token| expand_default_command_token(token))
                .collect::<Result<Vec<_>>>()?;
            command.extend(options.args.iter().cloned());
            if options.content.is_some() {
                command.extend(["--content".to_owned(), "<content>".to_owned()]);
            }
            Ok(command)
        }
    }
}

fn expand_default_command_token(token: &str) -> Result<String> {
    expand_default_command_token_in_root(token, &workspace_namespace_root())
}

fn expand_default_command_token_in_root(token: &str, root: &Path) -> Result<String> {
    let Some(path) = token.strip_prefix("@file:") else {
        return Ok(token.to_owned());
    };
    Ok(resolve_workspace_relative_file_under(path, root)?
        .display()
        .to_string())
}

fn default_run_goal(tool: &ToolSpec, args: &[String]) -> String {
    if args.is_empty() {
        return format!("run {}", tool.name);
    }
    format!("run {} with {}", tool.name, args.join(" "))
}

fn print_lines(label: &str, lines: &[String]) {
    for line in lines {
        println!("{label}> {line}");
    }
}

fn build_chat_request_history(
    history: &[ChatMessage],
    selected_tool_context: Option<String>,
    user_input: &str,
) -> Vec<ChatMessage> {
    let mut request_history = history.to_vec();
    if let Some(selected_tool_context) = selected_tool_context
        && !selected_tool_context.trim().is_empty()
    {
        append_system_context(&mut request_history, &selected_tool_context);
    }
    request_history.push(ChatMessage::new(MessageRole::User, user_input.to_owned()));
    request_history
}

fn merge_optional_contexts<const N: usize>(contexts: [Option<String>; N]) -> Option<String> {
    let merged = contexts
        .into_iter()
        .flatten()
        .filter(|context| !context.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");
    if merged.trim().is_empty() {
        None
    } else {
        Some(merged)
    }
}

fn selection_input_from_history(history: &[ChatMessage], current_input: &str) -> String {
    let mut user_messages = history
        .iter()
        .filter(|message| message.role == MessageRole::User)
        .rev()
        .take(2)
        .map(|message| message.content.trim().to_owned())
        .collect::<Vec<_>>();
    user_messages.reverse();
    user_messages.push(current_input.trim().to_owned());
    user_messages
        .into_iter()
        .filter(|message| !message.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn append_system_context(messages: &mut Vec<ChatMessage>, context: &str) {
    if let Some(system_message) = messages
        .iter_mut()
        .find(|message| message.role == MessageRole::System)
    {
        if !system_message.content.ends_with("\n\n") {
            system_message.content.push_str("\n\n");
        }
        system_message.content.push_str(context.trim());
        return;
    }

    messages.insert(
        0,
        ChatMessage::new(MessageRole::System, context.trim().to_owned()),
    );
}

fn route_tool_turn(
    registry: &ProviderRegistry,
    provider: &str,
    model: &str,
    user_turn: &str,
    selection: &ToolSelection,
    user_system: Option<&str>,
) -> Result<NaturalLanguageToolRoute> {
    let prompt = render_tool_router_prompt(selection, user_turn, user_system)?;
    let mut request = GenerateRequest::new(
        ModelRef::new(provider.to_owned(), model.to_owned()),
        vec![
            ChatMessage::new(MessageRole::System, prompt),
            ChatMessage::new(MessageRole::User, user_turn.to_owned()),
        ],
    );
    request.temperature = Some(0.0);
    request.max_output_tokens = Some(256);
    let response = registry
        .generate(&request)
        .map_err(|error| anyhow::anyhow!(error))?;
    parse_tool_route_response(&response.message.content)
}

fn parse_tool_route_response(content: &str) -> Result<NaturalLanguageToolRoute> {
    let json_text = extract_json_object(content).context("tool router did not return JSON")?;
    serde_json::from_str(json_text).context("failed to parse tool route JSON")
}

fn apply_tool_route(
    selection: &mut ToolSelection,
    catalog: &ToolCatalog,
    route: NaturalLanguageToolRoute,
) -> Result<()> {
    let _ = route.message.as_deref();
    let Some(tool_id) = route.tool_id.filter(|tool_id| !tool_id.trim().is_empty()) else {
        selection.selected = None;
        return Ok(());
    };
    let normalized = normalize_tool_selector(tool_id.trim());
    if let Some(tool) = catalog.get(&normalized) {
        selection.selected = Some(tool.clone());
        return Ok(());
    }
    bail!("tool router selected unknown tool: {tool_id}")
}

fn render_tool_execution_context(
    plan: &hc_toolchain::ToolExecutionPlan,
    outcome: &ToolExecutionOutcome,
) -> String {
    let observations = outcome
        .observations
        .iter()
        .take(80)
        .map(|line| format!("- {line}"))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "Executed tool for this user turn:\n- tool_id: {}\n- success: {}\n- summary: {}\n- command: {}\n- planned_tool_id: {}\n- observations:\n{}",
        outcome.tool_id,
        outcome.success,
        outcome.summary,
        outcome.command.join(" "),
        plan.tool_id,
        observations
    )
}

fn render_chat_error(error: &hc_llm::LlmError) -> String {
    let message = error.to_string();
    if message.contains("invalid chat setting") {
        return format!(
            "error> provider rejected the chat request: invalid chat setting. 已保留当前会话，可继续输入或 /clear 后重试。\nprovider> {message}"
        );
    }
    if is_timeout_error(error) {
        return format!(
            "error> provider request timed out. 这是当前 provider 的网络/响应超时，当前会话已保留；可以继续重试、切换 HC_LLM_PROVIDER/HC_LLM_MODEL，或稍后再试。\nprovider> {message}"
        );
    }
    format!("error> {message}")
}

fn render_router_warning(error: &anyhow::Error) -> String {
    let message = error.to_string();
    let lowered = message.to_ascii_lowercase();
    if lowered.contains("timed out") || lowered.contains("timeout") {
        return "warning> tool router timed out; continuing with candidate tools only.".to_owned();
    }
    format!("warning> tool router skipped: {message}")
}

fn load_cli_tool_catalog() -> Result<ToolCatalog> {
    let mut catalog = default_tool_catalog();
    if let Ok(custom_catalog) = tool_repository().load_catalog() {
        catalog.register_provider(&custom_catalog);
    }
    if let Ok(skill_catalog) = skill_repository().load_catalog() {
        catalog.register_provider(&skill_catalog);
    }
    Ok(catalog)
}

fn tool_repository() -> ToolRepository {
    ToolRepository::with_namespace(workspace_root(), runtime_namespace())
}

fn skill_repository() -> SkillRepository {
    SkillRepository::with_namespace(workspace_root(), runtime_namespace())
}

fn runtime_namespace() -> WorkspaceNamespace {
    let tenant_id = env::var("HC_TENANT_ID").unwrap_or_else(|_| "local".to_owned());
    let user_id = env::var("HC_USER_ID").unwrap_or_else(|_| "default".to_owned());
    WorkspaceNamespace::new(tenant_id, user_id)
}

fn workspace_root() -> PathBuf {
    env::var("HC_WORKSPACE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("workspace"))
}

fn workspace_namespace_root() -> PathBuf {
    let namespace = runtime_namespace();
    workspace_root()
        .join("tenants")
        .join(namespace.tenant_id)
        .join("users")
        .join(namespace.user_id)
}

fn split_chat_command(input: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    let mut quote_char = '\0';
    let mut escaped = false;

    for ch in input.chars() {
        if escaped {
            current.push(ch);
            escaped = false;
            continue;
        }
        if ch == '\\' {
            escaped = true;
            continue;
        }
        if in_quotes {
            if ch == quote_char {
                in_quotes = false;
            } else {
                current.push(ch);
            }
            continue;
        }
        if ch == '"' || ch == '\'' {
            in_quotes = true;
            quote_char = ch;
            continue;
        }
        if ch.is_whitespace() {
            if !current.is_empty() {
                tokens.push(current.clone());
                current.clear();
            }
            continue;
        }
        current.push(ch);
    }

    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

fn synthesize_tool_build_from_natural_language(
    registry: &ProviderRegistry,
    provider: &str,
    model: &str,
    input: &str,
    catalog: &ToolCatalog,
    user_system: Option<&str>,
) -> Result<NaturalLanguageToolBuild> {
    let prompt = render_tool_builder_prompt(catalog, user_system)?;

    let mut request = GenerateRequest::new(
        ModelRef::new(provider.to_owned(), model.to_owned()),
        vec![
            ChatMessage::new(MessageRole::System, prompt),
            ChatMessage::new(MessageRole::User, input.to_owned()),
        ],
    );
    request.temperature = Some(0.0);
    request.max_output_tokens = Some(2048);
    let response = registry
        .generate(&request)
        .map_err(|error| anyhow::anyhow!(error))?;
    match parse_tool_build_response(&response.message.content) {
        Ok(build) => Ok(build),
        Err(error) => {
            if let Some(command) = extract_create_tool_command(&response.message.content) {
                return build_from_create_tool_command(&command);
            }
            Err(error)
        }
    }
}

fn parse_tool_build_response(content: &str) -> Result<NaturalLanguageToolBuild> {
    let json_text = extract_json_object(content).context("LLM did not return a JSON object")?;
    serde_json::from_str(json_text).context("failed to parse tool creation JSON")
}

fn extract_json_object(content: &str) -> Option<&str> {
    let trimmed = content.trim();
    if trimmed.starts_with('{') && trimmed.ends_with('}') {
        return Some(trimmed);
    }
    let start = trimmed.find('{')?;
    let end = trimmed.rfind('}')?;
    if start <= end {
        Some(&trimmed[start..=end])
    } else {
        None
    }
}

fn try_execute_create_tool_command_from_response(content: &str) -> Result<Option<PathBuf>> {
    let Some(command) = extract_create_tool_command(content) else {
        return Ok(None);
    };
    handle_create_from_chat(&command).map(Some)
}

fn build_from_create_tool_command(command: &str) -> Result<NaturalLanguageToolBuild> {
    let args = split_chat_command(command);
    let options = parse_create_options(&args)?;
    Ok(NaturalLanguageToolBuild {
        action: "create_tool".to_owned(),
        message: None,
        tool: Some(NaturalLanguageToolDraft {
            id: options.id,
            name: options.name,
            description: options.description,
            execution_kind: Some(tool_execution_kind_label(&options.execution_kind).to_owned()),
            default_command: options.command,
            files: Vec::new(),
            tags: options.tags,
        }),
        skill: None,
    })
}

fn tool_execution_kind_label(kind: &ToolExecutionKind) -> &'static str {
    match kind {
        ToolExecutionKind::Script => "script",
        ToolExecutionKind::Workflow => "workflow",
        ToolExecutionKind::Cli => "cli",
        ToolExecutionKind::Service => "service",
        ToolExecutionKind::Builtin => "builtin",
    }
}

fn extract_create_tool_command(content: &str) -> Option<String> {
    for line in content.lines() {
        let mut candidate = line.trim();
        candidate = candidate
            .trim_start_matches('`')
            .trim_end_matches('`')
            .trim();
        if let Some(rest) = candidate.strip_prefix("$ ") {
            candidate = rest.trim();
        }
        if let Some(index) = candidate.find("/create-tool ") {
            let command = &candidate[index + "/create-tool ".len()..];
            return Some(trim_trailing_confirmation_text(command).to_owned());
        }
        if let Some(index) = candidate.find("hc-tool-cli create ") {
            let command = &candidate[index + "hc-tool-cli create ".len()..];
            return Some(trim_trailing_confirmation_text(command).to_owned());
        }
    }
    None
}

fn trim_trailing_confirmation_text(command: &str) -> &str {
    command
        .split("是否需要")
        .next()
        .unwrap_or(command)
        .split("需要我")
        .next()
        .unwrap_or(command)
        .split("要我")
        .next()
        .unwrap_or(command)
        .trim()
}

fn sanitize_model_response(content: &str) -> String {
    let without_think = strip_tag_blocks(content, "think");
    let without_minimax_tool = strip_tag_blocks(&without_think, "minimax:tool_call");
    let without_tool = strip_tag_blocks(&without_minimax_tool, "tool_call");
    let without_invoke = strip_tag_blocks(&without_tool, "invoke");
    without_invoke
        .lines()
        .filter(|line| {
            let trimmed = line.trim();
            !trimmed.starts_with("$SKILL")
                && !trimmed.starts_with("<parameter ")
                && !trimmed.starts_with("</parameter>")
        })
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_owned()
}

fn persist_response_artifacts(user_input: &str, content: &str) -> Result<Vec<PathBuf>> {
    let blocks = extract_code_blocks(content);
    if blocks.is_empty() {
        return Ok(Vec::new());
    }

    let artifact_dir = artifact_output_dir();
    fs::create_dir_all(&artifact_dir)
        .with_context(|| format!("failed to create {}", artifact_dir.display()))?;

    let mut paths = Vec::new();
    let mut index = 0usize;
    for block in blocks {
        let Some(extension) = code_block_extension(&block) else {
            continue;
        };
        if !looks_like_complete_artifact(&block, extension) {
            continue;
        }
        index += 1;
        let file_name = artifact_file_name(user_input, extension, index);
        let path = unique_artifact_path(&artifact_dir, &file_name);
        fs::write(&path, block.content.trim_start_matches('\n'))
            .with_context(|| format!("failed to write {}", path.display()))?;
        paths.push(path);
    }
    Ok(paths)
}

fn extract_code_blocks(content: &str) -> Vec<CodeBlock> {
    let mut blocks = Vec::new();
    let mut lines = content.lines();
    while let Some(line) = lines.next() {
        let trimmed = line.trim_start();
        let Some(info) = trimmed.strip_prefix("```") else {
            continue;
        };
        let language = info
            .split_whitespace()
            .next()
            .filter(|value| !value.is_empty())
            .map(|value| value.trim().to_ascii_lowercase());
        let mut body = String::new();
        for body_line in lines.by_ref() {
            if body_line.trim_start().starts_with("```") {
                break;
            }
            body.push_str(body_line);
            body.push('\n');
        }
        blocks.push(CodeBlock {
            language,
            content: body,
        });
    }
    blocks
}

fn code_block_extension(block: &CodeBlock) -> Option<&'static str> {
    match block.language.as_deref().unwrap_or_default() {
        "html" => Some("html"),
        "css" => Some("css"),
        "js" | "javascript" => Some("js"),
        "ts" | "typescript" => Some("ts"),
        "jsx" => Some("jsx"),
        "tsx" => Some("tsx"),
        "json" => Some("json"),
        "md" | "markdown" => Some("md"),
        _ => infer_code_extension(&block.content),
    }
}

fn infer_code_extension(content: &str) -> Option<&'static str> {
    let trimmed = content.trim_start();
    if trimmed.starts_with("<!DOCTYPE html") || trimmed.starts_with("<html") {
        return Some("html");
    }
    if trimmed.starts_with('{') || trimmed.starts_with('[') {
        return Some("json");
    }
    None
}

fn looks_like_complete_artifact(block: &CodeBlock, extension: &str) -> bool {
    let content = block.content.trim();
    if content.len() < 40 {
        return false;
    }
    match extension {
        "html" => {
            content.contains("<html")
                || content.contains("<body")
                || content.contains("<!DOCTYPE html")
        }
        "css" => content.contains('{') && content.contains('}'),
        "js" | "ts" | "jsx" | "tsx" => {
            content.contains("function")
                || content.contains("=>")
                || content.contains("import ")
                || content.contains("const ")
                || content.contains("let ")
                || content.contains("class ")
        }
        "json" => content.starts_with('{') || content.starts_with('['),
        "md" => content.lines().any(|line| line.starts_with('#')),
        _ => false,
    }
}

fn artifact_output_dir() -> PathBuf {
    let namespace = runtime_namespace();
    workspace_root()
        .join("tenants")
        .join(namespace.tenant_id)
        .join("users")
        .join(namespace.user_id)
        .join("artifacts")
}

fn artifact_file_name(user_input: &str, extension: &str, index: usize) -> String {
    let slug = artifact_slug(user_input);
    let suffix = if index > 1 {
        format!("-{index}")
    } else {
        String::new()
    };
    format!("{slug}{suffix}.{extension}")
}

fn artifact_slug(input: &str) -> String {
    let lowered = input.to_ascii_lowercase();
    let mut slug = lowered
        .split(|ch: char| !ch.is_ascii_alphanumeric())
        .filter(|part| !part.is_empty())
        .take(6)
        .collect::<Vec<_>>()
        .join("-");
    if slug.is_empty() {
        slug = "artifact".to_owned();
    }
    slug
}

fn unique_artifact_path(dir: &Path, file_name: &str) -> PathBuf {
    let candidate = dir.join(file_name);
    if !candidate.exists() {
        return candidate;
    }
    let stem = Path::new(file_name)
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("artifact");
    let extension = Path::new(file_name)
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("txt");
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    dir.join(format!("{stem}.{timestamp}.{extension}"))
}

fn strip_tag_blocks(content: &str, tag: &str) -> String {
    let open = format!("<{tag}");
    let close = format!("</{tag}>");
    let mut remaining = content;
    let mut output = String::new();

    while let Some(start) = remaining.find(&open) {
        output.push_str(&remaining[..start]);
        let after_open = &remaining[start..];
        let Some(end) = after_open.find(&close) else {
            return output.trim().to_owned();
        };
        remaining = &after_open[end + close.len()..];
    }

    output.push_str(remaining);
    output
}

fn default_registry() -> ProviderRegistry {
    default_registry_from_env()
}

fn render_tool_chat_system_prompt(
    catalog: &ToolCatalog,
    user_system: Option<&str>,
) -> Result<String> {
    render_prompt_template(
        load_tool_chat_prompt(&runtime_namespace())?,
        &[
            ("available_tools", render_available_tools(catalog)),
            ("selected_tool", String::new()),
            (
                "additional_system_guidance",
                render_optional_guidance(user_system),
            ),
        ],
    )
}

fn render_tool_builder_prompt(catalog: &ToolCatalog, user_system: Option<&str>) -> Result<String> {
    render_prompt_template(
        load_tool_natural_language_builder_prompt(&runtime_namespace())?,
        &[
            ("existing_tools", render_existing_tools(catalog)),
            (
                "additional_system_guidance",
                render_optional_guidance(user_system),
            ),
        ],
    )
}

fn render_tool_router_prompt(
    selection: &ToolSelection,
    user_turn: &str,
    user_system: Option<&str>,
) -> Result<String> {
    render_prompt_template(
        load_tool_router_prompt(&runtime_namespace())?,
        &[
            ("tool_candidates", render_tool_route_candidates(selection)),
            ("user_turn", user_turn.to_owned()),
            (
                "additional_system_guidance",
                render_optional_guidance(user_system),
            ),
        ],
    )
}

fn render_prompt_template(template: String, values: &[(&str, String)]) -> Result<String> {
    let mut rendered = template;
    for (key, value) in values {
        rendered = rendered.replace(&format!("{{{{{key}}}}}"), value);
    }
    Ok(rendered)
}

fn render_tool_route_candidates(selection: &ToolSelection) -> String {
    selection
        .candidates
        .iter()
        .map(|candidate| {
            format!(
                "- id={} | score={} | name={} | kind={:?} | composition={:?} | tags={} | description={} | default_command={}",
                candidate.tool.id,
                candidate.score,
                candidate.tool.name,
                candidate.tool.execution_kind,
                candidate.tool.composition,
                candidate.tool.tags.join(", "),
                compact_single_line(&candidate.tool.description, 600),
                candidate.tool.default_command.join(" ")
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn render_available_tools(catalog: &ToolCatalog) -> String {
    catalog
        .list()
        .into_iter()
        .map(|tool| {
            format!(
                "- {} | name={} | kind={:?} | composition={:?} | tags={} | description={} | default_command={}",
                tool.id,
                tool.name,
                tool.execution_kind,
                tool.composition,
                tool.tags.join(", "),
                tool.description,
                tool.default_command.join(" ")
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn render_tool_selection_context(selection: &ToolSelection) -> Option<String> {
    if selection.selected.is_none() && selection.candidates.is_empty() {
        return None;
    }

    let mut sections = Vec::new();
    if let Some(tool) = &selection.selected {
        sections.push(format!(
            "Selected tool for this user turn:\n{}",
            render_tool_context(tool)
        ));
    }

    let candidates = selection
        .candidates
        .iter()
        .map(|candidate| {
            format!(
                "- score={} | {}",
                candidate.score,
                render_tool_context(&candidate.tool).replace('\n', " | ")
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    if !candidates.trim().is_empty() {
        sections.push(format!("Tool candidates for this user turn:\n{candidates}"));
    }

    Some(sections.join("\n\n"))
}

fn render_tool_context(tool: &ToolSpec) -> String {
    let mut rendered = format!(
        "- id: {}\n- name: {}\n- kind: {:?}\n- composition: {:?}\n- description: {}\n- tags: {}\n- default_command: {}",
        tool.id,
        tool.name,
        tool.execution_kind,
        tool.composition,
        tool.description,
        tool.tags.join(", "),
        tool.default_command.join(" ")
    );
    if let Some(skill) = skill_profile_for_tool(tool)
        && !skill.instructions.trim().is_empty()
    {
        rendered.push_str("\n- skill_instructions: ");
        rendered.push_str(&compact_single_line(skill.instructions.trim(), 1200));
    }
    rendered
}

fn skill_profile_for_tool(tool: &ToolSpec) -> Option<SkillProfile> {
    if tool.composition != ToolComposition::Composite && !tool.tags.iter().any(|tag| tag == "skill")
    {
        return None;
    }
    skill_repository()
        .list_profiles()
        .ok()?
        .into_iter()
        .find(|skill| skill.resolved_tool_id() == tool.id || skill.id == tool.id)
}

fn compact_single_line(value: &str, max_chars: usize) -> String {
    let mut compact = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.chars().count() > max_chars {
        compact = compact.chars().take(max_chars).collect::<String>();
        compact.push_str("...");
    }
    compact
}

fn render_existing_tools(catalog: &ToolCatalog) -> String {
    catalog
        .list()
        .into_iter()
        .map(|tool| {
            format!(
                "- {}: {} command={}",
                tool.id,
                tool.description,
                tool.default_command.join(" ")
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn render_optional_guidance(user_system: Option<&str>) -> String {
    user_system
        .filter(|value| !value.trim().is_empty())
        .map(|value| format!("Additional system guidance:\n{}", value.trim()))
        .unwrap_or_default()
}

fn prompt_raw(editor: &mut DefaultEditor) -> Result<Option<String>> {
    match editor.readline("you> ") {
        Ok(input) => {
            if !input.trim().is_empty() {
                let _ = editor.add_history_entry(input.as_str());
            }
            Ok(Some(input))
        }
        Err(ReadlineError::Interrupted) => Ok(Some(String::new())),
        Err(ReadlineError::Eof) => Ok(None),
        Err(error) => Err(anyhow::Error::new(error)).context("failed to read interactive input"),
    }
}

fn env_file_path() -> Result<PathBuf> {
    Ok(env::current_dir()
        .context("failed to read current directory")?
        .join(".env"))
}

fn load_local_env_file() -> Result<()> {
    let env_path = env_file_path()?;
    if !env_path.exists() {
        return Ok(());
    }

    for (key, value) in read_env_map(&env_path)? {
        if env::var_os(&key).is_none() {
            unsafe { env::set_var(key, value) };
        }
    }

    Ok(())
}

fn read_env_map(path: &Path) -> Result<BTreeMap<String, String>> {
    if !path.exists() {
        return Ok(BTreeMap::new());
    }

    let content =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    let mut vars = BTreeMap::new();
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let Some((key, value)) = trimmed.split_once('=') else {
            continue;
        };
        vars.insert(key.trim().to_owned(), value.trim().to_owned());
    }
    Ok(vars)
}

fn default_provider() -> String {
    default_provider_from_env()
}

fn default_model() -> String {
    default_model_from_env()
}

fn is_help(value: &str) -> bool {
    matches!(value, "help" | "--help" | "-h")
}

fn print_help() {
    println!("hc-tool-cli                    # start tool-aware chat");
    println!("hc-tool-cli chat [--provider <id>] [--model <name>] [--system <text>]");
    println!(
        "hc-tool-cli create <tool-id> <name> --description <text> --command <token> [--command <token>] [--kind <cli|builtin|script|workflow|service>] [--tag <tag>] [--json]"
    );
    println!("hc-tool-cli list [--json]");
    println!("hc-tool-cli show <rg|cargo-test|tool-id> [--json]");
    println!("hc-tool-cli plan <auto|rg|cargo-test|tool-id> <goal...> [--json]");
    println!(
        "hc-tool-cli run <rg|tool.rg> <pattern> [extra rg args...] [--path <dir>] [--goal <text>] [--json]"
    );
    println!(
        "hc-tool-cli run <cargo-test|tool.cargo-test> [filter] [--package <pkg>] [--path <dir>] [--goal <text>] [--json]"
    );
    println!(
        "hc-tool-cli run <tool.local-file.read|tool.local-file.write|tool.local-dir.list> <path> [--content <text>] [--path <dir>] [--json]"
    );
}

#[cfg(test)]
mod tests {
    use super::{
        KeywordToolSelector, ToolSelector, build_chat_request_history,
        build_from_create_tool_command, code_block_extension, execute_builtin_tool,
        expand_default_command_token_in_root, extract_code_blocks, extract_create_tool_command,
        looks_like_complete_artifact, parse_tool_build_response, parse_tool_route_response,
        render_chat_error, render_tool_execution_context, render_tool_selection_context,
        sanitize_model_response, score_tool_for_goal, selection_input_from_history,
        skill_from_natural_language_draft, tool_from_natural_language_draft,
        try_execute_create_tool_command_from_response, write_generated_tool_files_under,
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
            description: "前端工程师红色系技能工具包，包含 React/Vue/TypeScript 开发能力"
                .to_owned(),
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
        let context = render_tool_selection_context(&selection)
            .expect("candidate context should be available");
        assert!(context.contains("Tool candidates"));
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
        let build = parse_tool_build_response(
            r#"{"action":"ignore","message":null,"tool":null,"skill":null}"#,
        )
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

        assert!(context.contains("Executed tool"));
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
        let root = unique_temp_dir("hc-tool-cli-generated-files");
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
        let root = unique_temp_dir("hc-tool-cli-file-tool");
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
        let root = unique_temp_dir("hc-tool-cli-dir-tool");
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
}
