use anyhow::Result;
use hc_intent::IntentResolution;
use hc_protocol::{ApiChatMessage, ChatRequest};

use crate::{
    ServiceConfig,
    chat::resolve_chat_agent_selection,
    room_routing::{
        PROVIDER_CHAT_FALLBACK, PROVIDER_MCP_TOOL, PROVIDER_PENDING_CONFIRMATION, PROVIDER_TIMED,
        RoomRoutingContext,
    },
    timed_turn::{TimedTurnPlan, resolve_timed_turn_plan},
    tool_turn::{
        ConfiguredMcpRoute, PendingToolExecutionPlan, resolve_configured_mcp_route_with_policy,
        resolve_persisted_pending_confirmation_plan,
    },
};

#[derive(Debug, Clone)]
pub enum TurnRoute {
    PendingConfirmation(PendingToolExecutionPlan),
    Timed(TimedTurnPlan),
    McpTool(ConfiguredMcpRoute),
    ChatFallback(ChatFallbackPlan),
}

#[derive(Debug, Clone, Default)]
pub struct ChatFallbackPlan {
    pub selected_agent_id: Option<String>,
    pub selected_domain_id: Option<String>,
    pub selection_reasoning: Option<String>,
}

#[derive(Debug, Clone)]
pub struct TurnCandidate {
    pub provider_id: &'static str,
    pub score: i32,
    pub reason: String,
    pub route: TurnRoute,
}

#[derive(Debug, Clone)]
pub struct TurnDecision {
    pub selected: TurnCandidate,
    pub considered: Vec<TurnCandidate>,
}

pub struct TurnRouterInput<'a> {
    pub config: &'a ServiceConfig,
    pub request: &'a ChatRequest,
    pub intent: &'a IntentResolution,
    pub room_routing: Option<&'a RoomRoutingContext>,
    pub history_for_match: &'a [ApiChatMessage],
}

pub trait TurnCandidateProvider: Send + Sync {
    fn id(&self) -> &'static str;
    fn propose(&self, input: &TurnRouterInput<'_>) -> Result<Option<TurnCandidate>>;
}

#[derive(Default)]
pub struct TurnProviderRegistry {
    providers: Vec<Box<dyn TurnCandidateProvider>>,
}

impl TurnProviderRegistry {
    pub fn with_builtin_defaults() -> Self {
        let mut registry = Self::default();
        registry.register(Box::new(PendingConfirmationProvider));
        registry.register(Box::new(TimedProvider));
        registry.register(Box::new(McpToolProvider));
        registry.register(Box::new(ChatFallbackProvider));
        registry
    }

    pub fn register(&mut self, provider: Box<dyn TurnCandidateProvider>) {
        self.providers.push(provider);
    }

    pub fn decide(&self, input: &TurnRouterInput<'_>) -> Result<TurnDecision> {
        let mut considered = Vec::new();
        for provider in &self.providers {
            if let Some(candidate) = provider.propose(input)? {
                considered.push(candidate);
            }
        }

        considered.sort_by(|left, right| {
            right
                .score
                .cmp(&left.score)
                .then_with(|| left.provider_id.cmp(right.provider_id))
        });
        let selected = considered
            .first()
            .cloned()
            .expect("chat fallback provider should always emit a candidate");
        Ok(TurnDecision {
            selected,
            considered,
        })
    }
}

struct PendingConfirmationProvider;

impl TurnCandidateProvider for PendingConfirmationProvider {
    fn id(&self) -> &'static str {
        PROVIDER_PENDING_CONFIRMATION
    }

    fn propose(&self, input: &TurnRouterInput<'_>) -> Result<Option<TurnCandidate>> {
        if !provider_enabled(input.room_routing, self.id()) {
            return Ok(None);
        }
        let Some(plan) = resolve_persisted_pending_confirmation_plan(input.config, input.request)?
        else {
            return Ok(None);
        };
        Ok(Some(TurnCandidate {
            provider_id: self.id(),
            score: provider_score(input.room_routing, self.id(), 4000),
            reason: "pending confirmation state matched current turn".to_owned(),
            route: TurnRoute::PendingConfirmation(plan),
        }))
    }
}

struct TimedProvider;

impl TurnCandidateProvider for TimedProvider {
    fn id(&self) -> &'static str {
        PROVIDER_TIMED
    }

    fn propose(&self, input: &TurnRouterInput<'_>) -> Result<Option<TurnCandidate>> {
        if !provider_enabled(input.room_routing, self.id()) {
            return Ok(None);
        }
        let Some(plan) = resolve_timed_turn_plan(
            input.config,
            input.request,
            input.intent,
            input.history_for_match,
        )?
        else {
            return Ok(None);
        };
        let reason = match &plan {
            TimedTurnPlan::Reminder { rule, .. } => format!("matched reminder rule {}", rule.id),
            TimedTurnPlan::Sequence { rule, values } => {
                format!(
                    "matched timed sequence rule {} ({} ticks)",
                    rule.id,
                    values.len()
                )
            }
        };
        Ok(Some(TurnCandidate {
            provider_id: self.id(),
            score: provider_score(input.room_routing, self.id(), 3000),
            reason,
            route: TurnRoute::Timed(plan),
        }))
    }
}

struct McpToolProvider;

impl TurnCandidateProvider for McpToolProvider {
    fn id(&self) -> &'static str {
        PROVIDER_MCP_TOOL
    }

    fn propose(&self, input: &TurnRouterInput<'_>) -> Result<Option<TurnCandidate>> {
        if !provider_enabled(input.room_routing, self.id()) {
            return Ok(None);
        }
        let Some(route) = resolve_configured_mcp_route_with_policy(
            input.config,
            input.request,
            input
                .room_routing
                .and_then(|context| context.allowed_tool_ids.as_ref()),
            input
                .room_routing
                .and_then(|context| context.provider_argument_override(self.id())),
            input
                .room_routing
                .map(|context| &context.tool_argument_overrides),
        )?
        else {
            return Ok(None);
        };
        let reason = if let Some(room) = input.room_routing {
            format!(
                "selected MCP tool {} within room {}",
                route.tool.id, room.room.id
            )
        } else {
            format!("selected MCP tool {}", route.tool.id)
        };
        Ok(Some(TurnCandidate {
            provider_id: self.id(),
            score: provider_score(input.room_routing, self.id(), 2000),
            reason,
            route: TurnRoute::McpTool(route),
        }))
    }
}

struct ChatFallbackProvider;

impl TurnCandidateProvider for ChatFallbackProvider {
    fn id(&self) -> &'static str {
        PROVIDER_CHAT_FALLBACK
    }

    fn propose(&self, input: &TurnRouterInput<'_>) -> Result<Option<TurnCandidate>> {
        let plan = resolve_chat_fallback_plan(input)?;
        let reason = if let Some(room) = input.room_routing {
            match plan.selection_reasoning.as_deref() {
                Some(selection_reasoning) => {
                    format!(
                        "fallback chat in room {} ({selection_reasoning})",
                        room.room.id
                    )
                }
                None => format!("fallback chat in room {}", room.room.id),
            }
        } else {
            plan.selection_reasoning
                .clone()
                .unwrap_or_else(|| "fallback chat".to_owned())
        };
        Ok(Some(TurnCandidate {
            provider_id: self.id(),
            score: provider_score(input.room_routing, self.id(), 0),
            reason,
            route: TurnRoute::ChatFallback(plan),
        }))
    }
}

fn resolve_chat_fallback_plan(input: &TurnRouterInput<'_>) -> Result<ChatFallbackPlan> {
    let selection = resolve_chat_agent_selection(input.config, input.request)?;
    Ok(ChatFallbackPlan {
        selected_agent_id: selection.selected_agent_id,
        selected_domain_id: selection.selected_domain_id,
        selection_reasoning: Some(selection.reasoning),
    })
}

fn provider_enabled(room_routing: Option<&RoomRoutingContext>, provider_id: &str) -> bool {
    room_routing.is_none_or(|context| context.allows_provider(provider_id))
}

fn provider_score(
    room_routing: Option<&RoomRoutingContext>,
    provider_id: &str,
    default_score: i32,
) -> i32 {
    default_score
        + room_routing
            .map(|context| context.provider_weight(provider_id))
            .unwrap_or_default()
}
