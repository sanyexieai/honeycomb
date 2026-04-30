use anyhow::Result;
use hc_agent::{AgentProfile, AgentRepository, DomainProfile, DomainRepository};
use hc_protocol::{
    AgentListResponse, AgentProfileSummary, AgentRouteCandidate, AgentRouteRequest,
    AgentRouteResponse, ApiNamespace, DomainListResponse, DomainProfileSummary,
};
use hc_store::store::WorkspaceNamespace;

use crate::ServiceConfig;

pub fn list_agents(config: &ServiceConfig, namespace: ApiNamespace) -> Result<AgentListResponse> {
    let repository = AgentRepository::with_namespace(
        config.workspace_root.clone(),
        WorkspaceNamespace::new(namespace.tenant_id, namespace.user_id),
    );
    let agents = repository
        .list_profiles()?
        .into_iter()
        .map(|profile| {
            let summary = profile.summary();
            AgentProfileSummary {
                id: summary.id,
                name: summary.name,
                kind: summary.kind,
                project_id: summary.project_id,
                domain_id: summary.domain_id,
                priority: summary.priority,
                intent_hints: summary.intent_hints,
                tool_refs: summary.tool_refs,
                memory_scope_refs: summary.memory_scope_refs,
                tags: summary.tags,
            }
        })
        .collect();
    Ok(AgentListResponse { agents })
}

pub fn list_domains(config: &ServiceConfig, namespace: ApiNamespace) -> Result<DomainListResponse> {
    let repository = DomainRepository::with_namespace(
        config.workspace_root.clone(),
        WorkspaceNamespace::new(namespace.tenant_id, namespace.user_id),
    );
    let domains = repository
        .list_profiles()?
        .into_iter()
        .map(|profile| {
            let summary = profile.summary();
            DomainProfileSummary {
                id: summary.id,
                name: summary.name,
                kind: summary.kind,
                project_id: summary.project_id,
                priority: summary.priority,
                intent_hints: summary.intent_hints,
                default_agent_id: summary.default_agent_id,
                tool_refs: summary.tool_refs,
                memory_scope_refs: summary.memory_scope_refs,
                tags: summary.tags,
            }
        })
        .collect();
    Ok(DomainListResponse { domains })
}

pub fn route_agent(
    config: &ServiceConfig,
    request: AgentRouteRequest,
) -> Result<AgentRouteResponse> {
    let namespace = WorkspaceNamespace::new(
        request.namespace.tenant_id.clone(),
        request.namespace.user_id.clone(),
    );
    let agent_repository =
        AgentRepository::with_namespace(config.workspace_root.clone(), namespace.clone());
    let domain_repository =
        DomainRepository::with_namespace(config.workspace_root.clone(), namespace);

    let agents = agent_repository.list_profiles()?;
    let domains = domain_repository.list_profiles()?;
    let input = request.input.to_lowercase();
    let limit = request.limit.unwrap_or(5).clamp(1, 20);

    let mut candidates: Vec<AgentRouteCandidate> = agents
        .iter()
        .map(|agent| score_agent(agent, &domains, &request, &input))
        .filter(|candidate| candidate.score > 0)
        .collect();

    if candidates.is_empty() {
        let fallback = agents
            .iter()
            .find(|agent| agent.id.ends_with(".chitchat"))
            .or_else(|| agents.iter().find(|agent| agent.id.ends_with(".router")))
            .or_else(|| agents.first());
        candidates = fallback
            .map(|agent| {
                vec![AgentRouteCandidate {
                    agent_id: agent.id.clone(),
                    domain_id: agent.domain_id.clone(),
                    score: 0,
                    reasons: vec!["fallback_agent".to_owned()],
                }]
            })
            .unwrap_or_default();
    }

    candidates.sort_by(|left, right| {
        right
            .score
            .cmp(&left.score)
            .then_with(|| left.agent_id.cmp(&right.agent_id))
    });
    candidates.truncate(limit);

    let selected = candidates.first();
    Ok(AgentRouteResponse {
        selected_agent_id: selected.map(|candidate| candidate.agent_id.clone()),
        selected_domain_id: selected.and_then(|candidate| candidate.domain_id.clone()),
        candidates,
    })
}

fn score_agent(
    agent: &AgentProfile,
    domains: &[DomainProfile],
    request: &AgentRouteRequest,
    input: &str,
) -> AgentRouteCandidate {
    let mut score = 0;
    let mut reasons = Vec::new();

    if request.active_agent_id.as_deref() == Some(agent.id.as_str()) {
        score += 100;
        reasons.push("active_agent".to_owned());
    }

    if let Some(request_domain_id) = request.domain_id.as_deref() {
        if agent.domain_id.as_deref() == Some(request_domain_id) {
            score += 60;
            reasons.push("requested_domain".to_owned());
        }
    }

    if let Some(request_project_id) = request.project_id.as_deref() {
        if agent.project_id.as_deref() == Some(request_project_id) {
            score += 15;
            reasons.push("requested_project".to_owned());
        }
    }

    for hint in &agent.intent_hints {
        if contains_hint(input, hint) {
            score += 50;
            reasons.push(format!("agent_hint:{hint}"));
        }
    }

    if let Some(domain) = agent
        .domain_id
        .as_deref()
        .and_then(|domain_id| domains.iter().find(|domain| domain.id == domain_id))
    {
        for hint in &domain.intent_hints {
            if contains_hint(input, hint) {
                score += 30;
                reasons.push(format!("domain_hint:{hint}"));
            }
        }
        if score > 0 {
            score += agent.priority / 10;
            score += domain.priority / 20;
            if domain.default_agent_id.as_deref() == Some(agent.id.as_str()) {
                score += 10;
                reasons.push("domain_default_agent".to_owned());
            }
        }
    }

    AgentRouteCandidate {
        agent_id: agent.id.clone(),
        domain_id: agent.domain_id.clone(),
        score,
        reasons,
    }
}

fn contains_hint(input: &str, hint: &str) -> bool {
    let hint = hint.trim().to_lowercase();
    !hint.is_empty() && input.contains(&hint)
}
