use std::{
    collections::BTreeMap,
    env, fs,
    io::{self, Write},
    path::{Path, PathBuf},
    thread,
    time::Duration,
};

use anyhow::{Context, Result, bail};
use hc_llm::{
    ChatMessage, GenerateRequest, GenerateResponse, MessageRole, ModelRef, ProviderRegistry,
    StreamChunk, default_base_url_for_provider, default_model_for_provider, default_model_from_env,
    default_provider_from_env, default_registry_from_env, provider_api_key_from_env,
    provider_api_key_var_name, provider_base_url_var_name, provider_preset, provider_presets,
};
use hc_log::CliLogger;
use std::sync::OnceLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RequestMode {
    Direct,
    Stream,
}

#[derive(Debug, Clone, Copy)]
struct OutputStyle {
    typewriter: bool,
    typewriter_delay_ms: u64,
}

static CLI_LOGGER: OnceLock<CliLogger> = OnceLock::new();

fn init_cli_log() {
    let logger = CliLogger::init_for_local_workspace_run(
        PathBuf::from("workspace"),
        "hc-llm-cli",
        "hc-llm-cli",
    );
    let _ = CLI_LOGGER.set(logger);
}

fn cli_logger() -> &'static CliLogger {
    CLI_LOGGER
        .get()
        .expect("cli log should be initialized before use")
}

fn main() -> Result<()> {
    load_local_env_file()?;
    init_cli_log();
    let args: Vec<String> = env::args().skip(1).collect();
    let mut configured_before = is_llm_configured();

    if args.is_empty() {
        if !configured_before {
            println!("No LLM configuration found. Starting setup wizard.");
            run_setup_wizard()?;
            load_local_env_file()?;
            configured_before = is_llm_configured();
        }
        if configured_before {
            let registry = default_registry();
            return handle_chat(&registry, &[]);
        }
        return Ok(());
    }

    let registry = default_registry();

    match args[0].as_str() {
        "config" => handle_config(&args[1..]),
        "providers" => print_supported_providers(),
        "chat" => handle_chat(&registry, &args[1..]),
        "generate" => handle_generate(&registry, &args[1..]),
        "help" | "--help" | "-h" => {
            print_help();
            Ok(())
        }
        other => bail!("unknown command: {other}"),
    }
}

fn default_registry() -> ProviderRegistry {
    default_registry_from_env()
}

fn handle_config(args: &[String]) -> Result<()> {
    match args {
        [target] if target == "llm" => run_setup_wizard(),
        [target, rest @ ..] if target == "llm" => handle_llm_config(rest),
        [target, rest @ ..] if target == "openai" => handle_openai_alias_config(rest),
        [target] if target == "show" => {
            let env_path = env_file_path()?;
            if !env_path.exists() {
                println!("no .env file at {}", env_path.display());
                return Ok(());
            }
            let vars = read_env_map(&env_path)?;
            for key in [
                "HC_LLM_PROVIDER",
                "HC_LLM_MODEL_TYPE",
                "HC_LLM_MODEL",
                "HC_LLM_API_KEY",
                "HC_LLM_BASE_URL",
            ] {
                if let Some(value) = vars.get(key) {
                    let shown = if key.ends_with("API_KEY") {
                        redact_secret(value)
                    } else {
                        value.clone()
                    };
                    println!("{key}={shown}");
                }
            }
            Ok(())
        }
        _ => {
            print_config_help();
            Ok(())
        }
    }
}

fn handle_openai_alias_config(args: &[String]) -> Result<()> {
    let mut forwarded = vec!["--provider".to_owned(), "openai".to_owned()];
    forwarded.extend(args.iter().cloned());
    handle_llm_config(&forwarded)
}

fn handle_llm_config(args: &[String]) -> Result<()> {
    if args.is_empty() {
        return run_setup_wizard();
    }

    let mut api_key: Option<String> = None;
    let mut base_url: Option<String> = None;
    let mut provider: Option<String> = None;
    let mut model: Option<String> = None;
    let mut model_type: Option<String> = None;

    let mut index = 0usize;
    while index < args.len() {
        match args[index].as_str() {
            "--provider" => {
                provider = Some(
                    args.get(index + 1)
                        .cloned()
                        .context("missing value for --provider")?,
                );
                index += 2;
            }
            "--model" => {
                model = Some(
                    args.get(index + 1)
                        .cloned()
                        .context("missing value for --model")?,
                );
                index += 2;
            }
            "--model-type" => {
                model_type = Some(
                    args.get(index + 1)
                        .cloned()
                        .context("missing value for --model-type")?,
                );
                index += 2;
            }
            "--api-key" => {
                api_key = Some(
                    args.get(index + 1)
                        .cloned()
                        .context("missing value for --api-key")?,
                );
                index += 2;
            }
            "--base-url" => {
                base_url = Some(
                    args.get(index + 1)
                        .cloned()
                        .context("missing value for --base-url")?,
                );
                index += 2;
            }
            other => bail!("unknown config option: {other}"),
        }
    }

    let env_path = env_file_path()?;
    let mut vars = read_env_map(&env_path)?;
    let provider = normalize_provider(&provider.unwrap_or_else(|| {
        vars.get("HC_LLM_PROVIDER")
            .cloned()
            .unwrap_or_else(|| "openai".to_owned())
    }))?;
    let model_type = normalize_model_type(&model_type.unwrap_or_else(|| {
        vars.get("HC_LLM_MODEL_TYPE")
            .cloned()
            .unwrap_or_else(|| "balanced".to_owned())
    }))?;
    let model = model.unwrap_or_else(|| {
        vars.get("HC_LLM_MODEL")
            .cloned()
            .unwrap_or_else(|| default_model_for_provider(&provider, &model_type))
    });

    vars.insert("HC_LLM_PROVIDER".to_owned(), provider);
    vars.insert("HC_LLM_MODEL_TYPE".to_owned(), model_type);
    vars.insert("HC_LLM_MODEL".to_owned(), model);

    if let Some(api_key) = api_key {
        vars.insert("HC_LLM_API_KEY".to_owned(), api_key);
    }
    let base_url = base_url.unwrap_or_else(|| {
        vars.get("HC_LLM_BASE_URL")
            .cloned()
            .unwrap_or_else(|| default_base_url_for_provider(vars["HC_LLM_PROVIDER"].as_str()))
    });
    vars.insert("HC_LLM_BASE_URL".to_owned(), base_url);

    if !vars.contains_key("HC_LLM_API_KEY") {
        bail!(
            "missing HC_LLM_API_KEY. Use: hc-llm-cli config llm --provider <id> --api-key <key> [--base-url <url>]"
        );
    }

    write_env_map(&env_path, &vars)?;
    println!("saved LLM configuration to {}", env_path.display());
    if let Some(provider) = vars.get("HC_LLM_PROVIDER") {
        println!("HC_LLM_PROVIDER={provider}");
    }
    if let Some(model_type) = vars.get("HC_LLM_MODEL_TYPE") {
        println!("HC_LLM_MODEL_TYPE={model_type}");
    }
    if let Some(model) = vars.get("HC_LLM_MODEL") {
        println!("HC_LLM_MODEL={model}");
    }
    println!(
        "HC_LLM_API_KEY={}",
        redact_secret(vars.get("HC_LLM_API_KEY").unwrap())
    );
    if let Some(base_url) = vars.get("HC_LLM_BASE_URL") {
        println!("HC_LLM_BASE_URL={base_url}");
    }
    Ok(())
}

fn run_setup_wizard() -> Result<()> {
    let env_path = env_file_path()?;
    let mut vars = read_env_map(&env_path)?;

    let current_provider = normalize_provider(
        &vars
            .get("HC_LLM_PROVIDER")
            .cloned()
            .unwrap_or_else(|| "openai".to_owned()),
    )?;
    let provider = prompt_provider(&current_provider)?;

    let current_model_type = normalize_model_type(
        &vars
            .get("HC_LLM_MODEL_TYPE")
            .cloned()
            .unwrap_or_else(|| "balanced".to_owned()),
    )?;
    let model_type = prompt_model_type(&current_model_type)?;

    let current_base_url = vars
        .get("HC_LLM_BASE_URL")
        .cloned()
        .or_else(|| env::var(provider_base_url_var_name(&provider)).ok())
        .unwrap_or_else(|| default_base_url_for_provider(&provider));
    let api_key_hint = vars
        .get("HC_LLM_API_KEY")
        .cloned()
        .or_else(|| env::var(provider_api_key_var_name(&provider)).ok());

    println!(
        "API key{}:",
        api_key_hint
            .as_deref()
            .map(|value| format!(" [{}]", redact_secret(value)))
            .unwrap_or_default()
    );
    let api_key_input = prompt_raw("> ")?;
    let api_key = if api_key_input.trim().is_empty() {
        api_key_hint.context("API key is required")?
    } else {
        api_key_input.trim().to_owned()
    };

    let base_url = prompt_with_default("Base URL", &current_base_url)?;
    let recommended_model = default_model_for_provider(&provider, &model_type);
    let current_model = vars
        .get("HC_LLM_MODEL")
        .cloned()
        .unwrap_or_else(|| recommended_model.clone());
    println!("Recommended model for {provider}/{model_type}: {recommended_model}");
    let model = prompt_with_default("Model", &current_model)?;

    vars.insert("HC_LLM_PROVIDER".to_owned(), provider);
    vars.insert("HC_LLM_MODEL_TYPE".to_owned(), model_type);
    vars.insert("HC_LLM_MODEL".to_owned(), model);
    vars.insert("HC_LLM_API_KEY".to_owned(), api_key);
    vars.insert("HC_LLM_BASE_URL".to_owned(), base_url);

    write_env_map(&env_path, &vars)?;
    println!("saved LLM configuration to {}", env_path.display());
    println!(
        "HC_LLM_PROVIDER={}",
        vars.get("HC_LLM_PROVIDER").cloned().unwrap_or_default()
    );
    println!(
        "HC_LLM_MODEL_TYPE={}",
        vars.get("HC_LLM_MODEL_TYPE").cloned().unwrap_or_default()
    );
    println!(
        "HC_LLM_MODEL={}",
        vars.get("HC_LLM_MODEL").cloned().unwrap_or_default()
    );
    println!(
        "HC_LLM_API_KEY={}",
        redact_secret(
            vars.get("HC_LLM_API_KEY")
                .map(String::as_str)
                .unwrap_or_default()
        )
    );
    println!(
        "HC_LLM_BASE_URL={}",
        vars.get("HC_LLM_BASE_URL").cloned().unwrap_or_default()
    );
    Ok(())
}

fn handle_generate(registry: &ProviderRegistry, args: &[String]) -> Result<()> {
    if args.is_empty() {
        bail!(
            "usage: hc-llm-cli generate <prompt> [--provider <id>] [--model <name>] [--system <text>] [--json] [--request-mode <direct|stream>] [--stream] [--direct] [--typewriter] [--typewriter-delay-ms <n>]"
        );
    }

    let mut provider = default_provider();
    let mut model = default_model();
    let mut system_message: Option<String> = None;
    let mut json = false;
    let mut prompt_parts = Vec::new();
    let mut request_mode = default_request_mode();
    let mut output_style = OutputStyle {
        typewriter: false,
        typewriter_delay_ms: default_typewriter_delay_ms(),
    };

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
            "--json" => {
                json = true;
                index += 1;
            }
            "--request-mode" => {
                request_mode = parse_request_mode(
                    args.get(index + 1)
                        .context("missing value for --request-mode")?,
                )?;
                index += 2;
            }
            "--stream" => {
                request_mode = RequestMode::Stream;
                index += 1;
            }
            "--direct" => {
                request_mode = RequestMode::Direct;
                index += 1;
            }
            "--typewriter" => {
                output_style.typewriter = true;
                index += 1;
            }
            "--typewriter-delay-ms" => {
                output_style.typewriter_delay_ms = args
                    .get(index + 1)
                    .context("missing value for --typewriter-delay-ms")?
                    .parse::<u64>()
                    .context("invalid value for --typewriter-delay-ms")?;
                index += 2;
            }
            value => {
                prompt_parts.push(value.to_owned());
                index += 1;
            }
        }
    }

    if prompt_parts.is_empty() {
        bail!("missing prompt");
    }

    let mut messages = Vec::new();
    if let Some(system_message) = system_message {
        messages.push(ChatMessage::new(MessageRole::System, system_message));
    }
    messages.push(ChatMessage::new(MessageRole::User, prompt_parts.join(" ")));

    let request = GenerateRequest::new(ModelRef::new(provider, model), messages);
    let response = generate_with_mode(registry, &request, request_mode, output_style, json)?;

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&response).context("failed to serialize response")?
        );
    } else {
        println!();
    }

    Ok(())
}

fn handle_chat(registry: &ProviderRegistry, args: &[String]) -> Result<()> {
    let mut provider = default_provider();
    let mut model = default_model();
    let mut system_message = env::var("HC_LLM_SYSTEM").ok();
    let mut request_mode = default_request_mode();
    let mut output_style = OutputStyle {
        typewriter: true,
        typewriter_delay_ms: default_typewriter_delay_ms(),
    };

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
            "--request-mode" => {
                request_mode = parse_request_mode(
                    args.get(index + 1)
                        .context("missing value for --request-mode")?,
                )?;
                index += 2;
            }
            "--stream" => {
                request_mode = RequestMode::Stream;
                index += 1;
            }
            "--direct" => {
                request_mode = RequestMode::Direct;
                index += 1;
            }
            "--typewriter" => {
                output_style.typewriter = true;
                index += 1;
            }
            "--no-typewriter" => {
                output_style.typewriter = false;
                index += 1;
            }
            "--typewriter-delay-ms" => {
                output_style.typewriter_delay_ms = args
                    .get(index + 1)
                    .context("missing value for --typewriter-delay-ms")?
                    .parse::<u64>()
                    .context("invalid value for --typewriter-delay-ms")?;
                index += 2;
            }
            other => bail!("unknown chat option: {other}"),
        }
    }

    println!("hc-llm chat");
    println!(
        "provider={provider} model={model} request_mode={}",
        request_mode_label(request_mode)
    );
    println!("Type /help for commands, /quit to exit.");

    let mut history = Vec::new();
    if let Some(system_message) = &system_message {
        history.push(ChatMessage::new(
            MessageRole::System,
            system_message.clone(),
        ));
    }

    loop {
        print!("you> ");
        io::stdout().flush().context("failed to flush stdout")?;
        let input = prompt_raw("")?;
        let trimmed = input.trim();
        if trimmed.is_empty() {
            continue;
        }
        match trimmed {
            "/quit" | "/exit" => break,
            "/help" => {
                println!("/help");
                println!("/clear");
                println!("/system <text>");
                println!("/quit");
                continue;
            }
            "/clear" => {
                history.clear();
                if let Some(system_message) = &system_message {
                    history.push(ChatMessage::new(
                        MessageRole::System,
                        system_message.clone(),
                    ));
                }
                println!("history cleared");
                continue;
            }
            _ if trimmed.starts_with("/system ") => {
                let value = trimmed
                    .strip_prefix("/system ")
                    .map(str::trim)
                    .unwrap_or_default();
                system_message = if value.is_empty() {
                    None
                } else {
                    Some(value.to_owned())
                };
                history.clear();
                if let Some(system_message) = &system_message {
                    history.push(ChatMessage::new(
                        MessageRole::System,
                        system_message.clone(),
                    ));
                }
                println!("system prompt updated");
                continue;
            }
            _ => {}
        }

        history.push(ChatMessage::new(MessageRole::User, trimmed.to_owned()));
        let request = GenerateRequest::new(
            ModelRef::new(provider.clone(), model.clone()),
            history.clone(),
        );
        print!("assistant> ");
        io::stdout().flush().context("failed to flush stdout")?;
        match generate_with_mode(registry, &request, request_mode, output_style, false) {
            Ok(response) => {
                println!();
                history.push(response.message);
            }
            Err(error) => {
                println!();
                println!("error> {error}");
                let _ = history.pop();
            }
        }
    }

    Ok(())
}

fn print_help() {
    println!("hc-llm-cli");
    println!("hc-llm-cli                    # setup wizard if needed, otherwise start chat");
    println!(
        "hc-llm-cli chat [--provider <id>] [--model <name>] [--system <text>] [--request-mode <direct|stream>] [--stream] [--direct] [--typewriter] [--no-typewriter] [--typewriter-delay-ms <n>]"
    );
    println!(
        "hc-llm-cli config llm --provider <id> [--model-type <type>] [--model <name>] --api-key <key> [--base-url <url>]"
    );
    println!("hc-llm-cli config openai --api-key <key> [--base-url <url>]");
    println!("hc-llm-cli config show");
    println!("hc-llm-cli providers");
    println!(
        "hc-llm-cli generate <prompt> [--provider <id>] [--model <name>] [--system <text>] [--json] [--request-mode <direct|stream>] [--stream] [--direct] [--typewriter] [--typewriter-delay-ms <n>]"
    );
}

fn print_config_help() {
    println!(
        "hc-llm-cli config llm --provider <id> [--model-type <type>] [--model <name>] --api-key <key> [--base-url <url>]"
    );
    println!("hc-llm-cli config openai --api-key <key> [--base-url <url>]");
    println!("hc-llm-cli config show");
}

fn is_llm_configured() -> bool {
    provider_api_key_from_env(&default_provider()).is_some()
}

fn matches_openai_not_configured(error: &hc_llm::LlmError) -> bool {
    matches!(error, hc_llm::LlmError::ProviderNotFound(provider) if provider == "openai")
}

fn default_provider() -> String {
    default_provider_from_env()
}

fn default_model() -> String {
    default_model_from_env()
}

fn default_request_mode() -> RequestMode {
    env::var("HC_LLM_REQUEST_MODE")
        .ok()
        .and_then(|value| parse_request_mode(&value).ok())
        .unwrap_or(RequestMode::Direct)
}

fn default_typewriter_delay_ms() -> u64 {
    env::var("HC_LLM_TYPEWRITER_DELAY_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(12)
}

fn parse_request_mode(value: &str) -> Result<RequestMode> {
    match value.trim().to_ascii_lowercase().as_str() {
        "direct" | "sync" => Ok(RequestMode::Direct),
        "stream" | "streaming" => Ok(RequestMode::Stream),
        other => bail!("unsupported request mode: {other}. supported modes: direct, stream"),
    }
}

fn request_mode_label(mode: RequestMode) -> &'static str {
    match mode {
        RequestMode::Direct => "direct",
        RequestMode::Stream => "stream",
    }
}

fn generate_with_guidance(
    registry: &ProviderRegistry,
    request: &GenerateRequest,
) -> Result<hc_llm::GenerateResponse> {
    registry.generate(request).map_err(|error| {
        if request.model.provider == "openai" && matches_openai_not_configured(&error) {
            anyhow::anyhow!(
                "{error}\nconfigure it with: hc-llm-cli config llm --provider openai --model-type balanced --api-key <key> [--base-url <url>]"
            )
        } else {
            anyhow::anyhow!(error)
        }
    })
}

fn generate_with_mode(
    registry: &ProviderRegistry,
    request: &GenerateRequest,
    request_mode: RequestMode,
    output_style: OutputStyle,
    json: bool,
) -> Result<GenerateResponse> {
    match request_mode {
        RequestMode::Direct => {
            let response = generate_with_guidance(registry, request)?;
            if !json {
                render_output(&response.message.content, output_style)?;
            }
            Ok(response)
        }
        RequestMode::Stream => generate_stream_with_guidance(registry, request, output_style, json),
    }
}

fn generate_stream_with_guidance(
    registry: &ProviderRegistry,
    request: &GenerateRequest,
    output_style: OutputStyle,
    json: bool,
) -> Result<GenerateResponse> {
    let mut callback = |chunk: StreamChunk| -> Result<(), hc_llm::LlmError> {
        if !json {
            render_output(&chunk.delta, output_style)
                .map_err(|error| hc_llm::LlmError::ProviderFailure(error.to_string()))?;
        }
        Ok(())
    };

    registry.generate_stream(request, &mut callback).map_err(|error| {
        if request.model.provider == "openai" && matches_openai_not_configured(&error) {
            anyhow::anyhow!(
                "{error}\nconfigure it with: hc-llm-cli config llm --provider openai --model-type balanced --api-key <key> [--base-url <url>]"
            )
        } else {
            anyhow::anyhow!(error)
        }
    })
}

fn render_output(text: &str, output_style: OutputStyle) -> Result<()> {
    if output_style.typewriter {
        for character in text.chars() {
            print!("{character}");
            io::stdout().flush().context("failed to flush stdout")?;
            if output_style.typewriter_delay_ms > 0 {
                thread::sleep(Duration::from_millis(output_style.typewriter_delay_ms));
            }
        }
    } else {
        print!("{text}");
        io::stdout().flush().context("failed to flush stdout")?;
    }
    Ok(())
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

fn write_env_map(path: &Path, vars: &BTreeMap<String, String>) -> Result<()> {
    let mut lines = Vec::new();
    for (key, value) in vars {
        lines.push(format!("{key}={value}"));
    }
    let content = if lines.is_empty() {
        String::new()
    } else {
        format!("{}\n", lines.join("\n"))
    };
    fs::write(path, content).with_context(|| format!("failed to write {}", path.display()))?;
    Ok(())
}

fn redact_secret(value: &str) -> String {
    if value.len() <= 8 {
        return "********".to_owned();
    }
    format!("{}...{}", &value[..4], &value[value.len() - 4..])
}

fn prompt_with_default(label: &str, default: &str) -> Result<String> {
    println!("{label} [{default}]:");
    let input = prompt_raw("> ")?;
    if input.trim().is_empty() {
        Ok(default.to_owned())
    } else {
        Ok(input.trim().to_owned())
    }
}

fn prompt_raw(prompt: &str) -> Result<String> {
    cli_logger().set_active_prompt(prompt);
    print!("{prompt}");
    io::stdout().flush().context("failed to flush stdout")?;
    let mut input = String::new();
    let result = io::stdin()
        .read_line(&mut input)
        .context("failed to read stdin");
    cli_logger().clear_active_prompt();
    result?;
    Ok(input)
}

fn normalize_provider(provider: &str) -> Result<String> {
    let trimmed = provider.trim();
    if let Some(preset) = provider_preset(trimmed) {
        return Ok(preset.id.to_owned());
    }

    let supported = provider_presets()
        .iter()
        .map(|preset| preset.id)
        .collect::<Vec<_>>()
        .join(", ");
    bail!("unsupported provider: {trimmed}. supported providers: {supported}")
}

fn normalize_model_type(model_type: &str) -> Result<String> {
    match model_type.trim().to_ascii_lowercase().as_str() {
        "balanced" => Ok("balanced".to_owned()),
        "fast" => Ok("fast".to_owned()),
        "coding" => Ok("coding".to_owned()),
        other => {
            bail!("unsupported model type: {other}. supported model types: balanced, fast, coding")
        }
    }
}

fn prompt_provider(current_provider: &str) -> Result<String> {
    println!("Select provider:");
    for (index, preset) in provider_presets().iter().enumerate() {
        let marker = if preset.id == current_provider {
            "*"
        } else {
            " "
        };
        println!(
            "  {}. {} ({}) {}",
            index + 1,
            preset.id,
            preset.display_name,
            marker
        );
    }
    println!("Provider [{}]:", current_provider);
    let input = prompt_raw("> ")?;
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Ok(current_provider.to_owned());
    }
    if let Ok(index) = trimmed.parse::<usize>() {
        if let Some(preset) = provider_presets().get(index.saturating_sub(1)) {
            return Ok(preset.id.to_owned());
        }
    }
    normalize_provider(trimmed)
}

fn prompt_model_type(current_model_type: &str) -> Result<String> {
    let choices = ["balanced", "fast", "coding"];
    println!("Select model type:");
    for (index, choice) in choices.iter().enumerate() {
        let marker = if *choice == current_model_type {
            "*"
        } else {
            " "
        };
        println!("  {}. {} {}", index + 1, choice, marker);
    }
    println!("Model type [{}]:", current_model_type);
    let input = prompt_raw("> ")?;
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Ok(current_model_type.to_owned());
    }
    if let Ok(index) = trimmed.parse::<usize>() {
        if let Some(choice) = choices.get(index.saturating_sub(1)) {
            return Ok((*choice).to_owned());
        }
    }
    normalize_model_type(trimmed)
}

fn print_supported_providers() -> Result<()> {
    for preset in provider_presets() {
        println!(
            "{} {} base_url={} balanced={} fast={} coding={}",
            preset.id,
            preset.display_name,
            preset.default_base_url,
            preset.balanced_model,
            preset.fast_model,
            preset.coding_model
        );
    }
    Ok(())
}
