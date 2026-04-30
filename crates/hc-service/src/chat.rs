use anyhow::{Context, Result, anyhow, bail};
use hc_agent::{
    AgentKind, AgentProfile, AgentRepository, DomainKind, DomainProfile, DomainRepository,
};
use hc_context::{
    ContextMemoryQuery, ContextRequest, DefaultContextComposer, MemoryKind, MemoryNamespace,
    MemoryScope, PromptPolicy, WorkspaceMemoryRetriever, generate_with_context,
    load_context_memory_system_prompt, load_context_memory_usage_policy_prompt, memory_kind_label,
    memory_scope_label, workspace_namespace_from_memory_namespace,
};
use hc_llm::{
    ChatMessage, GenerateRequest, MessageRole, ModelRef, default_model_from_env,
    default_provider_from_env, default_registry_from_env,
};
use hc_protocol::{
    AgentRouteRequest, ApiChatMessage, ApiMemoryQuery, ApiMessageRole, ApiNamespace, ChatRequest,
    ChatResponse, MemoryRef,
};
use hc_store::store::WorkspaceNamespace;

use crate::{ServiceConfig, agent::route_agent};

pub fn handle_chat_request(config: &ServiceConfig, request: ChatRequest) -> Result<ChatResponse> {
    let request = normalize_chat_request(request);
    let memory_namespace = memory_namespace_from_api(&request.memory.namespace);
    let workspace_namespace = workspace_namespace_from_memory_namespace(&memory_namespace);
    let agent_context = resolve_agent_context(config, &request)?;
    let model = ModelRef::new(
        request
            .provider
            .clone()
            .unwrap_or_else(default_provider_from_env),
        request.model.clone().unwrap_or_else(default_model_from_env),
    );
    let messages = request_messages(&request)?;
    let mut generation = GenerateRequest::new(model.clone(), messages);
    generation.temperature = request.temperature;
    generation.max_output_tokens = request.max_output_tokens;

    let memory_query =
        build_memory_query(memory_namespace, &request.memory, request.input.clone())?;
    let base_system_prompt = match request.system_prompt.clone() {
        Some(system_prompt) if !system_prompt.trim().is_empty() => system_prompt,
        _ => load_context_memory_system_prompt(&workspace_namespace)?,
    };
    let system_prompt = compose_agent_system_prompt(base_system_prompt, agent_context.as_ref());
    let context_request = ContextRequest::new(generation)
        .with_memory_query(memory_query)
        .with_system_prompt(system_prompt)
        .with_prompt_policy(PromptPolicy::new(
            "Memory Usage Policy",
            load_context_memory_usage_policy_prompt(&workspace_namespace)?,
        ));

    let registry = default_registry_from_env();
    let retriever = WorkspaceMemoryRetriever::new(&config.workspace_root, workspace_namespace);
    let composer = DefaultContextComposer;
    let response = generate_with_context(&registry, &retriever, &composer, &context_request)?;

    Ok(ChatResponse {
        message: api_message_from_llm(response.response.message),
        model: response.response.model.model,
        provider: response.response.model.provider,
        tenant_id: Some(request.memory.namespace.tenant_id.clone()),
        user_id: Some(request.memory.namespace.user_id.clone()),
        session_id: request.session_id.clone(),
        selected_agent_id: agent_context
            .as_ref()
            .map(|context| context.agent.id.clone()),
        selected_domain_id: agent_context
            .as_ref()
            .and_then(|context| context.agent.domain_id.clone()),
        recalled_memories: response
            .recalled_memories
            .into_iter()
            .map(memory_ref_from_retrieved)
            .collect(),
        synthesized_prompt_asset_count: response.synthesized_prompt_assets.len(),
    })
}

fn normalize_chat_request(mut request: ChatRequest) -> ChatRequest {
    if let Some(tenant_id) = normalized_optional_string(request.tenant_id.take()) {
        request.memory.namespace.tenant_id = tenant_id;
    }
    if let Some(user_id) = normalized_optional_string(request.user_id.take()) {
        request.memory.namespace.user_id = user_id;
    }
    request.tenant_id = Some(request.memory.namespace.tenant_id.clone());
    request.user_id = Some(request.memory.namespace.user_id.clone());
    request.session_id = normalized_optional_string(request.session_id.take())
        .or_else(|| Some(default_session_id(&request.memory.namespace)));
    request
}

fn normalized_optional_string(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn default_session_id(namespace: &ApiNamespace) -> String {
    format!(
        "session.{}.{}.default",
        namespace.tenant_id, namespace.user_id
    )
}

#[derive(Debug, Clone)]
struct ResolvedAgentContext {
    agent: AgentProfile,
    domain: Option<DomainProfile>,
}

fn resolve_agent_context(
    config: &ServiceConfig,
    request: &ChatRequest,
) -> Result<Option<ResolvedAgentContext>> {
    let namespace = request.memory.namespace.clone();
    let workspace_namespace =
        WorkspaceNamespace::new(namespace.tenant_id.clone(), namespace.user_id.clone());
    let agent_repository =
        AgentRepository::with_namespace(config.workspace_root.clone(), workspace_namespace.clone());
    let domain_repository =
        DomainRepository::with_namespace(config.workspace_root.clone(), workspace_namespace);

    let agents = agent_repository.list_profiles()?;
    if agents.is_empty() {
        return Ok(None);
    }
    let domains = domain_repository.list_profiles()?;

    let selected_agent_id = if let Some(agent_id) = request.agent_id.as_deref() {
        Some(agent_id.to_owned())
    } else {
        let input = routing_input(request)?;
        route_agent(
            config,
            AgentRouteRequest {
                input,
                namespace,
                project_id: None,
                domain_id: request.domain_id.clone(),
                active_agent_id: request.active_agent_id.clone(),
                active_task_id: request.active_task_id.clone(),
                limit: Some(1),
            },
        )?
        .selected_agent_id
    };

    let Some(selected_agent_id) = selected_agent_id else {
        return Ok(None);
    };
    let agent = agents
        .into_iter()
        .find(|agent| agent.id == selected_agent_id)
        .with_context(|| format!("selected agent profile not found: {selected_agent_id}"))?;
    let domain = agent
        .domain_id
        .as_deref()
        .and_then(|domain_id| domains.into_iter().find(|domain| domain.id == domain_id));

    Ok(Some(ResolvedAgentContext { agent, domain }))
}

fn routing_input(request: &ChatRequest) -> Result<String> {
    if let Some(input) = request.input.as_deref()
        && !input.trim().is_empty()
    {
        return Ok(input.trim().to_owned());
    }
    request
        .messages
        .iter()
        .rev()
        .find(|message| message.role == ApiMessageRole::User && !message.content.trim().is_empty())
        .map(|message| message.content.trim().to_owned())
        .ok_or_else(|| anyhow!("chat request requires input or messages"))
}

fn compose_agent_system_prompt(
    base_system_prompt: String,
    context: Option<&ResolvedAgentContext>,
) -> String {
    let Some(context) = context else {
        return base_system_prompt;
    };

    let mut sections = vec![
        base_system_prompt.trim().to_owned(),
        format!(
            "[Selected Agent]\nid: {}\nname: {}\nkind: {}\nproject_id: {}\ndomain_id: {}\npriority: {}",
            context.agent.id,
            context.agent.name,
            agent_kind_label(&context.agent.kind),
            context.agent.project_id.as_deref().unwrap_or(""),
            context.agent.domain_id.as_deref().unwrap_or(""),
            context.agent.priority
        ),
    ];

    if let Some(domain) = &context.domain {
        sections.push(format!(
            "[Selected Domain]\nid: {}\nname: {}\nkind: {}\npriority: {}\n{}",
            domain.id,
            domain.name,
            domain_kind_label(&domain.kind),
            domain.priority,
            domain.description
        ));
    }

    if !context.agent.tool_refs.is_empty() {
        sections.push(format!(
            "[Available Tool References]\n{}",
            context.agent.tool_refs.join("\n")
        ));
    }
    if !context.agent.memory_scope_refs.is_empty() {
        sections.push(format!(
            "[Memory Scope References]\n{}",
            context.agent.memory_scope_refs.join("\n")
        ));
    }
    if !context.agent.instructions.trim().is_empty() {
        sections.push(format!(
            "[Agent Instructions]\n{}",
            context.agent.instructions.trim()
        ));
    }

    sections.join("\n\n")
}

fn agent_kind_label(kind: &AgentKind) -> &'static str {
    match kind {
        AgentKind::DomainService => "domain_service",
        AgentKind::TaskRole => "task_role",
        AgentKind::Router => "router",
        AgentKind::Guard => "guard",
        AgentKind::Other => "other",
    }
}

fn domain_kind_label(kind: &DomainKind) -> &'static str {
    match kind {
        DomainKind::Service => "service",
        DomainKind::ProjectArea => "project_area",
        DomainKind::Safety => "safety",
        DomainKind::Other => "other",
    }
}

fn request_messages(request: &ChatRequest) -> Result<Vec<ChatMessage>> {
    let mut messages = request
        .messages
        .iter()
        .map(llm_message_from_api)
        .collect::<Vec<_>>();
    if let Some(input) = request.input.as_deref()
        && !input.trim().is_empty()
    {
        messages.push(ChatMessage::new(MessageRole::User, input.trim().to_owned()));
    }
    if messages.is_empty() {
        bail!("chat request requires input or messages");
    }
    Ok(messages)
}

fn build_memory_query(
    namespace: MemoryNamespace,
    memory: &ApiMemoryQuery,
    fallback_text: Option<String>,
) -> Result<ContextMemoryQuery> {
    let mut query = ContextMemoryQuery::default().for_namespace(namespace);
    let text = memory
        .text
        .clone()
        .or(fallback_text)
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty());
    if let Some(text) = text {
        query = query.with_text(text);
    }
    query = query.with_limit(memory.limit.unwrap_or(8).max(1));
    if let Some(scope) = memory.scope.as_deref() {
        query = query.with_scope(parse_scope(scope)?);
    }
    if let Some(kind) = memory.kind.as_deref() {
        query.memory_query.kind = Some(parse_kind(kind)?);
    }
    if let Some(tag) = memory.tag.as_deref()
        && !tag.trim().is_empty()
    {
        query = query.with_tag(tag.trim().to_owned());
    }
    Ok(query)
}

fn llm_message_from_api(message: &ApiChatMessage) -> ChatMessage {
    let role = match message.role {
        ApiMessageRole::System => MessageRole::System,
        ApiMessageRole::User => MessageRole::User,
        ApiMessageRole::Assistant => MessageRole::Assistant,
        ApiMessageRole::Tool => MessageRole::Tool,
    };
    let mut chat_message = ChatMessage::new(role, message.content.clone());
    if let Some(name) = &message.name {
        chat_message = chat_message.named(name.clone());
    }
    chat_message
}

fn api_message_from_llm(message: ChatMessage) -> ApiChatMessage {
    let role = match message.role {
        MessageRole::System => ApiMessageRole::System,
        MessageRole::User => ApiMessageRole::User,
        MessageRole::Assistant => ApiMessageRole::Assistant,
        MessageRole::Tool => ApiMessageRole::Tool,
    };
    ApiChatMessage {
        role,
        content: message.content,
        name: message.name,
    }
}

fn memory_ref_from_retrieved(memory: hc_context::RetrievedMemory) -> MemoryRef {
    MemoryRef {
        id: memory.id,
        title: memory.title,
        summary: memory.summary,
        scope: memory_scope_label(&memory.scope).to_owned(),
        kind: memory_kind_label(&memory.kind).to_owned(),
        source_kind: memory.source_kind,
        confidence_milli: memory.confidence_milli,
        tags: memory.tags,
        room_id: memory.room_id,
    }
}

fn memory_namespace_from_api(namespace: &ApiNamespace) -> MemoryNamespace {
    MemoryNamespace::new(namespace.tenant_id.clone(), namespace.user_id.clone())
}

fn parse_scope(value: &str) -> Result<MemoryScope> {
    match value.trim().to_ascii_lowercase().as_str() {
        "global" => Ok(MemoryScope::Global),
        "persona" => Ok(MemoryScope::Persona),
        "session" => Ok(MemoryScope::Session),
        "instance" => Ok(MemoryScope::Instance),
        "project" => Ok(MemoryScope::Project),
        "task" => Ok(MemoryScope::Task),
        other => bail!("unsupported memory scope: {other}"),
    }
}

fn parse_kind(value: &str) -> Result<MemoryKind> {
    match value.trim().to_ascii_lowercase().as_str() {
        "summary" => Ok(MemoryKind::Summary),
        "decision" => Ok(MemoryKind::Decision),
        "preference" => Ok(MemoryKind::Preference),
        "knowledge" => Ok(MemoryKind::Knowledge),
        "workflow_memory" | "workflow-memory" => Ok(MemoryKind::WorkflowMemory),
        other => bail!("unsupported memory kind: {other}"),
    }
}

pub fn concise_error(error: &anyhow::Error) -> String {
    error
        .chain()
        .next()
        .map(|cause| cause.to_string())
        .unwrap_or_else(|| anyhow!("unknown error").to_string())
}
