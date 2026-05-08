use std::collections::{BTreeMap, BTreeSet};

use anyhow::Result;
use hc_memory::{
    MemoryNamespace, MemoryRoom, MemoryRoomRepository, ResolvedRoomCapabilities,
    RoomCapabilityResolver, RoomRoutingConfig,
};
use hc_protocol::ChatRequest;
use hc_store::store::WorkspaceNamespace;
use serde_json::{Map, Value};

use crate::ServiceConfig;

pub const PROVIDER_PENDING_CONFIRMATION: &str = "pending_confirmation";
pub const PROVIDER_TIMED: &str = "timed";
pub const PROVIDER_MCP_TOOL: &str = "mcp_tool";
pub const PROVIDER_CHAT_FALLBACK: &str = "chat_fallback";

/// Room-derived routing inputs for a single turn.
///
/// The first rollout intentionally keeps this lightweight and behavior-preserving.
/// It gives the turn pipeline one shared place to read room-scoped tools, skills,
/// and capabilities before we migrate to a full candidate-based router.
#[derive(Debug, Clone)]
pub struct RoomRoutingContext {
    pub room: MemoryRoom,
    pub resolved: ResolvedRoomCapabilities,
    pub enabled_providers: BTreeSet<String>,
    pub allowed_tool_ids: Option<BTreeSet<String>>,
    pub provider_weights: BTreeMap<String, i32>,
    pub provider_argument_overrides: BTreeMap<String, Map<String, Value>>,
    pub tool_argument_overrides: BTreeMap<String, Map<String, Value>>,
    pub capability_ids: BTreeSet<String>,
    pub skill_ids: BTreeSet<String>,
}

#[derive(Debug, Clone)]
pub struct RoomRoutingExplain {
    pub room_id: String,
    pub enabled_providers: Vec<String>,
    pub provider_weights: BTreeMap<String, i32>,
    pub capability_ids: Vec<String>,
    pub skill_ids: Vec<String>,
    pub allowed_tool_ids: Vec<String>,
    pub provider_argument_override_keys: BTreeMap<String, Vec<String>>,
    pub tool_argument_override_keys: BTreeMap<String, Vec<String>>,
}

/// Capability / tool / skill id lists for HTTP responses and tracing (sorted via `BTreeSet` iteration).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RoomResponseCapabilityLists {
    pub capabilities: Vec<String>,
    pub tools: Vec<String>,
    pub skills: Vec<String>,
}

impl RoomRoutingContext {
    /// Same field semantics as `hc-api` `room_capabilities_used` / `room_tools_used` / `room_skills_used`.
    #[must_use]
    pub fn response_capability_lists(&self) -> RoomResponseCapabilityLists {
        RoomResponseCapabilityLists {
            capabilities: self.capability_ids.iter().cloned().collect(),
            tools: self
                .allowed_tool_ids
                .as_ref()
                .map(|ids| ids.iter().cloned().collect())
                .unwrap_or_default(),
            skills: self.skill_ids.iter().cloned().collect(),
        }
    }

    pub fn allows_provider(&self, provider_id: &str) -> bool {
        self.enabled_providers.contains(provider_id)
    }

    pub fn provider_weight(&self, provider_id: &str) -> i32 {
        self.provider_weights
            .get(provider_id)
            .copied()
            .unwrap_or_default()
    }

    pub fn provider_argument_override(&self, provider_id: &str) -> Option<&Map<String, Value>> {
        self.provider_argument_overrides.get(provider_id)
    }
}

/// Task-layer room id doubles as task scope id for swarm task-binding (ADR-004).
#[inline]
#[must_use]
pub fn task_id_hint_from_room_routing(ctx: &RoomRoutingContext) -> Option<String> {
    use hc_memory::MemoryLayer;
    (ctx.room.layer == MemoryLayer::Task).then_some(ctx.room.id.clone())
}

pub fn resolve_room_routing_context(
    config: &ServiceConfig,
    request: &ChatRequest,
) -> Result<Option<RoomRoutingContext>> {
    let Some(room_id) = request
        .room_id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
    else {
        return Ok(None);
    };

    let namespace = normalized_memory_namespace(request);
    let workspace_namespace = WorkspaceNamespace::new(&namespace.tenant_id, &namespace.user_id);
    let repository =
        MemoryRoomRepository::with_namespace(config.workspace_root.clone(), workspace_namespace);
    let Some(room) = repository.get_room_by_id(room_id)? else {
        return Ok(None);
    };

    let resolver = RoomCapabilityResolver::new(namespace);
    let resolved = resolver.resolve_room_capabilities(&room)?;
    let routing = room.room_config.routing.clone();
    let allowed_tool_ids = build_allowed_tool_ids(&resolved, &routing);
    let tool_argument_overrides =
        build_tool_argument_overrides(&resolved, allowed_tool_ids.as_ref());
    let capability_ids = build_filtered_ids(
        resolved
            .capabilities
            .iter()
            .map(|capability| capability.capability_ref.id.clone())
            .collect::<BTreeSet<_>>(),
        &routing.capability_whitelist,
        &routing.capability_blacklist,
    );
    let skill_ids = build_filtered_ids(
        resolved
            .skills
            .iter()
            .map(|skill| skill.skill_ref.id.clone())
            .collect::<BTreeSet<_>>(),
        &routing.skill_whitelist,
        &routing.skill_blacklist,
    );
    let enabled_providers = build_enabled_providers(&routing);

    Ok(Some(RoomRoutingContext {
        room,
        resolved,
        enabled_providers,
        allowed_tool_ids,
        provider_weights: routing.provider_weights.clone(),
        provider_argument_overrides: routing.provider_argument_overrides.clone(),
        tool_argument_overrides,
        capability_ids,
        skill_ids,
    }))
}

pub fn resolve_room_routing_explain(
    config: &ServiceConfig,
    request: &ChatRequest,
) -> Result<Option<RoomRoutingExplain>> {
    Ok(resolve_room_routing_context(config, request)?
        .as_ref()
        .map(room_routing_explain_from_context))
}

pub fn room_routing_explain_from_context(context: &RoomRoutingContext) -> RoomRoutingExplain {
    RoomRoutingExplain {
        room_id: context.room.id.clone(),
        enabled_providers: context.enabled_providers.iter().cloned().collect(),
        provider_weights: context.provider_weights.clone(),
        capability_ids: context.capability_ids.iter().cloned().collect(),
        skill_ids: context.skill_ids.iter().cloned().collect(),
        allowed_tool_ids: context
            .allowed_tool_ids
            .clone()
            .unwrap_or_default()
            .into_iter()
            .collect(),
        provider_argument_override_keys: context
            .provider_argument_overrides
            .iter()
            .map(|(provider_id, args)| (provider_id.clone(), args.keys().cloned().collect()))
            .collect(),
        tool_argument_override_keys: context
            .tool_argument_overrides
            .iter()
            .map(|(tool_id, args)| (tool_id.clone(), args.keys().cloned().collect()))
            .collect(),
    }
}

fn build_allowed_tool_ids(
    resolved: &ResolvedRoomCapabilities,
    routing: &RoomRoutingConfig,
) -> Option<BTreeSet<String>> {
    let mut allowed_tool_ids = (!resolved.tools.is_empty()).then(|| {
        resolved
            .tools
            .iter()
            .map(|tool| tool.tool_ref.id.clone())
            .collect::<BTreeSet<_>>()
    });

    if !routing.tool_whitelist.is_empty() {
        let whitelist = routing
            .tool_whitelist
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>();
        allowed_tool_ids = Some(match allowed_tool_ids.take() {
            Some(existing) => existing.intersection(&whitelist).cloned().collect(),
            None => whitelist,
        });
    }

    if let Some(allowed) = allowed_tool_ids.as_mut() {
        for tool_id in &routing.tool_blacklist {
            allowed.remove(tool_id);
        }
    }

    allowed_tool_ids
}

fn build_tool_argument_overrides(
    resolved: &ResolvedRoomCapabilities,
    allowed_tool_ids: Option<&BTreeSet<String>>,
) -> BTreeMap<String, Map<String, Value>> {
    resolved
        .tools
        .iter()
        .filter(|tool| allowed_tool_ids.is_none_or(|allowed| allowed.contains(&tool.tool_ref.id)))
        .filter_map(|tool| {
            tool.tool_ref
                .args_override
                .clone()
                .map(|args| (tool.tool_ref.id.clone(), args))
        })
        .collect()
}

fn build_filtered_ids(
    mut values: BTreeSet<String>,
    whitelist: &[String],
    blacklist: &[String],
) -> BTreeSet<String> {
    if !whitelist.is_empty() {
        let whitelist = whitelist.iter().cloned().collect::<BTreeSet<_>>();
        values = values.intersection(&whitelist).cloned().collect();
    }
    for value in blacklist {
        values.remove(value);
    }
    values
}

pub fn default_enabled_providers() -> BTreeSet<String> {
    CORE_PROVIDER_IDS.into_iter().map(str::to_owned).collect()
}

fn build_enabled_providers(routing: &RoomRoutingConfig) -> BTreeSet<String> {
    let mut enabled = if routing.enabled_providers.is_empty() {
        default_enabled_providers()
    } else {
        routing.enabled_providers.iter().cloned().collect()
    };

    for provider_id in &routing.disabled_providers {
        enabled.remove(provider_id);
    }

    if enabled.is_empty() {
        enabled.insert(PROVIDER_CHAT_FALLBACK.to_owned());
    }

    enabled
}

pub const CORE_PROVIDER_IDS: [&str; 4] = [
    PROVIDER_PENDING_CONFIRMATION,
    PROVIDER_TIMED,
    PROVIDER_MCP_TOOL,
    PROVIDER_CHAT_FALLBACK,
];

fn normalized_memory_namespace(request: &ChatRequest) -> MemoryNamespace {
    let tenant_id = request
        .tenant_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(hc_protocol::DEFAULT_TENANT_ID);
    let user_id = request
        .user_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(hc_protocol::DEFAULT_USER_ID);
    MemoryNamespace::new(tenant_id, user_id)
}

#[cfg(test)]
mod tests {
    use super::{
        PROVIDER_CHAT_FALLBACK, PROVIDER_MCP_TOOL, PROVIDER_TIMED, RoomResponseCapabilityLists,
        RoomRoutingContext, build_allowed_tool_ids, build_enabled_providers, build_filtered_ids,
        task_id_hint_from_room_routing,
    };
    use hc_memory::{
        MemoryLayer, MemoryRoom, ResolvedRoomCapabilities, ResolvedTool, RoomRoutingConfig,
    };
    use std::collections::{BTreeMap, BTreeSet};

    #[test]
    fn routing_config_filters_tools_with_whitelist_and_blacklist() {
        let mut resolved = ResolvedRoomCapabilities::new("room.test");
        resolved
            .tools
            .push(ResolvedTool::auto_discovered("tool.alpha"));
        resolved
            .tools
            .push(ResolvedTool::auto_discovered("tool.beta"));

        let routing = RoomRoutingConfig::new()
            .with_tool_whitelist("tool.alpha")
            .with_tool_whitelist("tool.gamma")
            .with_tool_blacklist("tool.gamma");

        let allowed = build_allowed_tool_ids(&resolved, &routing).expect("filtered tools");
        assert!(allowed.contains("tool.alpha"));
        assert!(!allowed.contains("tool.beta"));
        assert!(!allowed.contains("tool.gamma"));
    }

    #[test]
    fn routing_config_applies_provider_enable_disable() {
        let routing = RoomRoutingConfig::new()
            .with_enabled_provider(PROVIDER_MCP_TOOL)
            .with_enabled_provider(PROVIDER_TIMED)
            .with_disabled_provider(PROVIDER_TIMED);

        let enabled = build_enabled_providers(&routing);
        assert!(enabled.contains(PROVIDER_MCP_TOOL));
        assert!(!enabled.contains(PROVIDER_TIMED));
        assert!(!enabled.contains(PROVIDER_CHAT_FALLBACK));
    }

    #[test]
    fn routing_config_keeps_chat_fallback_when_everything_disabled() {
        let routing = RoomRoutingConfig::new().with_disabled_provider(PROVIDER_MCP_TOOL);
        let enabled = build_enabled_providers(&routing);
        assert!(enabled.contains(PROVIDER_CHAT_FALLBACK));
    }

    #[test]
    fn routing_config_filters_capabilities_and_skills() {
        let capabilities = build_filtered_ids(
            [
                "capability.alpha".to_string(),
                "capability.beta".to_string(),
            ]
            .into_iter()
            .collect(),
            &["capability.alpha".to_string()],
            &["capability.beta".to_string()],
        );
        let skills = build_filtered_ids(
            ["skill.alpha".to_string(), "skill.beta".to_string()]
                .into_iter()
                .collect(),
            &["skill.alpha".to_string(), "skill.beta".to_string()],
            &["skill.beta".to_string()],
        );

        assert!(capabilities.contains("capability.alpha"));
        assert!(!capabilities.contains("capability.beta"));
        assert!(skills.contains("skill.alpha"));
        assert!(!skills.contains("skill.beta"));
    }

    fn stub_room_routing_ctx(room_id: &str, layer: MemoryLayer) -> RoomRoutingContext {
        RoomRoutingContext {
            room: MemoryRoom::new(room_id, layer, "t", "s"),
            resolved: ResolvedRoomCapabilities::new(room_id),
            enabled_providers: BTreeSet::new(),
            allowed_tool_ids: None,
            provider_weights: BTreeMap::new(),
            provider_argument_overrides: BTreeMap::new(),
            tool_argument_overrides: BTreeMap::new(),
            capability_ids: BTreeSet::new(),
            skill_ids: BTreeSet::new(),
        }
    }

    #[test]
    fn response_capability_lists_sorted_ids() {
        let mut ctx = stub_room_routing_ctx("room", MemoryLayer::Chat);
        ctx.capability_ids = ["z", "a"].into_iter().map(String::from).collect();
        ctx.skill_ids = ["s_z", "s_a"].into_iter().map(String::from).collect();
        ctx.allowed_tool_ids = Some(["t_b", "t_a"].into_iter().map(String::from).collect());
        assert_eq!(
            ctx.response_capability_lists(),
            RoomResponseCapabilityLists {
                capabilities: vec!["a".to_string(), "z".to_string()],
                tools: vec!["t_a".to_string(), "t_b".to_string()],
                skills: vec!["s_a".to_string(), "s_z".to_string()],
            }
        );

        ctx.allowed_tool_ids = None;
        assert!(ctx.response_capability_lists().tools.is_empty());
    }

    #[test]
    fn task_id_hint_is_room_id_only_for_task_layer() {
        assert_eq!(
            task_id_hint_from_room_routing(&stub_room_routing_ctx("task.demo", MemoryLayer::Task))
                .as_deref(),
            Some("task.demo")
        );
        assert!(
            task_id_hint_from_room_routing(&stub_room_routing_ctx("room.chat", MemoryLayer::Chat))
                .is_none()
        );
    }
}
