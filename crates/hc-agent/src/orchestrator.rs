use anyhow::Result;
use hc_core::{
    MessageKind, MessageRecord, MessageRoute, ParticipationClaim, RuntimeCommand,
    RuntimeCommandResult, RuntimeSupervisor, SpeakingGrant,
};
use hc_responder::{ReplyRequest, ResponderBackend};
use serde::{Deserialize, Serialize};

use crate::swarm_routing;
use crate::{AgentPlan, IncubationReport, MaterializedAgent, TaskRequest};
use hc_protocol::swarm::{
    RoutingDecisionRecord, RoutingTier, SwarmRoutingBindingSnapshot, TaskBindingDecisionRecord,
};
use hc_trace::{TraceEvent, emit_trace};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SwarmMessageClassification {
    pub routing: RoutingDecisionRecord,
    pub task_binding: TaskBindingDecisionRecord,
}

impl SwarmMessageClassification {
    #[must_use]
    pub fn routing_binding_snapshot(&self) -> SwarmRoutingBindingSnapshot {
        SwarmRoutingBindingSnapshot::new(self.routing.clone(), self.task_binding.clone())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct NominationCycleOutcome {
    pub grant: Option<SpeakingGrant>,
    pub swarm: Option<SwarmMessageClassification>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct AgentOrchestrator;

impl AgentOrchestrator {
    pub fn new() -> Self {
        Self
    }

    pub fn describe_bootstrap(&self, task: &TaskRequest, plan: &AgentPlan) -> String {
        format!(
            "task={} seeds={}",
            task.id,
            plan.seeds
                .iter()
                .map(|seed| seed.role.as_str())
                .collect::<Vec<_>>()
                .join(",")
        )
    }

    pub fn summarize_incubation(&self, report: &IncubationReport) -> String {
        format!(
            "task={} instance={} observations={}",
            report.task_id,
            report.instance_id,
            report.observations.len()
        )
    }

    pub fn suggest_claims_for_message(
        &self,
        runtime: &RuntimeSupervisor,
        agents: &[MaterializedAgent],
        message: &MessageRecord,
        timestamp_ms: u64,
    ) -> Result<Vec<ParticipationClaim>> {
        let nomination = runtime.nomination_for_message(&message.id)?;
        let round = nomination.current_round;

        let claims = agents
            .iter()
            .filter(|agent| agent.persona.collaboration_rules.auto_claim)
            .filter_map(|agent| {
                let score = role_match_score(&agent.seed.role, &message.body, &message.route);
                (score > 0.0).then(|| {
                    ParticipationClaim::new(
                        &message.id,
                        &agent.binding.instance_id,
                        score,
                        round,
                        timestamp_ms,
                    )
                    .with_reason(format!("role={} matched message topic", agent.seed.role))
                })
            })
            .collect();

        Ok(claims)
    }

    pub fn submit_suggested_claims(
        &self,
        runtime: &mut RuntimeSupervisor,
        claims: &[ParticipationClaim],
    ) -> Result<()> {
        for claim in claims {
            runtime.dispatch(RuntimeCommand::SubmitParticipationClaim {
                claim: claim.clone(),
            })?;
        }
        Ok(())
    }

    pub fn run_nomination_cycle(
        &self,
        runtime: &mut RuntimeSupervisor,
        agents: &[MaterializedAgent],
        message: &MessageRecord,
        timestamp_ms: u64,
        conversation_active_task_id: Option<&str>,
        task_id_hint: Option<&str>,
    ) -> Result<NominationCycleOutcome> {
        let swarm = if matches!(message.kind, MessageKind::Chat) {
            let routing = swarm_routing::decide_routing_tier(&message.body);
            let task_binding = swarm_routing::decide_task_binding(
                &routing,
                conversation_active_task_id,
                task_id_hint,
            );
            swarm_routing::emit_swarm_message_routing(
                &routing,
                &task_binding,
                &message.id,
                &message.session_id,
            );
            Some(SwarmMessageClassification {
                routing,
                task_binding,
            })
        } else {
            None
        };

        let skip_message_nomination = swarm
            .as_ref()
            .is_some_and(|s| matches!(s.routing.routing_tier, RoutingTier::L2 | RoutingTier::L3));

        let grant = if skip_message_nomination {
            if let Some(ref classified) = swarm {
                emit_trace(
                    TraceEvent::info(
                        "hc-agent",
                        "swarm",
                        "message_nomination_skipped_for_routing_tier",
                        format!(
                            "{}: skip message-level nomination",
                            classified.routing.routing_tier
                        ),
                    )
                    .with_field("message_id", message.id.clone())
                    .with_field("session_id", message.session_id.clone())
                    .with_field("routing_tier", classified.routing.routing_tier.as_str()),
                );
            }
            None
        } else {
            let claims = self.suggest_claims_for_message(runtime, agents, message, timestamp_ms)?;
            self.submit_suggested_claims(runtime, &claims)?;

            let round = runtime.nomination_for_message(&message.id)?.current_round;
            let result = runtime.dispatch(RuntimeCommand::ResolveSpeakingGrant {
                message_id: message.id.clone(),
                round,
            })?;
            let RuntimeCommandResult::SpeakingGrant(grant) = result else {
                anyhow::bail!("unexpected runtime result while resolving speaking grant");
            };
            grant
        };

        Ok(NominationCycleOutcome { grant, swarm })
    }

    pub fn build_reply_request_for_grant(
        &self,
        runtime: &RuntimeSupervisor,
        agents: &[MaterializedAgent],
        grant: &SpeakingGrant,
    ) -> Result<ReplyRequest> {
        let message = runtime
            .state()
            .messages
            .iter()
            .find(|message| message.id == grant.message_id)
            .ok_or_else(|| anyhow::anyhow!("message not found for grant: {}", grant.message_id))?;
        let agent = agents
            .iter()
            .find(|agent| agent.binding.instance_id == grant.instance_id)
            .ok_or_else(|| {
                anyhow::anyhow!("agent not found for instance: {}", grant.instance_id)
            })?;
        let responder = agent.binding.responder.as_ref().ok_or_else(|| {
            anyhow::anyhow!(
                "responder binding missing for instance: {}",
                grant.instance_id
            )
        })?;

        Ok(ReplyRequest {
            source_message_id: message.id.clone(),
            source_session_id: message.session_id.clone(),
            source_from_instance_id: message.from.clone(),
            source_body: message.body.clone(),
            replying_instance_id: grant.instance_id.clone(),
            replying_agent_name: agent.persona.name.clone(),
            replying_role: agent.persona.role.clone(),
            responder: responder.clone(),
        })
    }

    pub fn generate_and_post_reply(
        &self,
        backend: &dyn ResponderBackend,
        runtime: &mut RuntimeSupervisor,
        agents: &[MaterializedAgent],
        grant: &SpeakingGrant,
    ) -> Result<MessageRecord> {
        let request = self.build_reply_request_for_grant(runtime, agents, grant)?;
        let response = backend.generate_reply(&request)?;

        let result = runtime.dispatch(RuntimeCommand::PostMessage {
            session_id: request.source_session_id.clone(),
            from: grant.instance_id.clone(),
            route: MessageRoute::Direct {
                to: request.source_from_instance_id.clone(),
            },
            kind: MessageKind::Chat,
            body: response.body,
            reply_to: Some(request.source_message_id),
        })?;
        let RuntimeCommandResult::Message(message) = result else {
            anyhow::bail!("unexpected runtime result while posting generated reply");
        };
        Ok(message)
    }

    pub fn generate_and_post_direct_reply(
        &self,
        backend: &dyn ResponderBackend,
        runtime: &mut RuntimeSupervisor,
        agents: &[MaterializedAgent],
        message_id: &str,
        replying_instance_id: &str,
    ) -> Result<MessageRecord> {
        let request =
            self.build_direct_reply_request(runtime, agents, message_id, replying_instance_id)?;
        let response = backend.generate_reply(&request)?;

        let result = runtime.dispatch(RuntimeCommand::PostMessage {
            session_id: request.source_session_id.clone(),
            from: replying_instance_id.to_owned(),
            route: MessageRoute::Direct {
                to: request.source_from_instance_id.clone(),
            },
            kind: MessageKind::Chat,
            body: response.body,
            reply_to: Some(request.source_message_id),
        })?;
        let RuntimeCommandResult::Message(message) = result else {
            anyhow::bail!("unexpected runtime result while posting generated direct reply");
        };
        Ok(message)
    }

    pub fn build_direct_reply_request(
        &self,
        runtime: &RuntimeSupervisor,
        agents: &[MaterializedAgent],
        message_id: &str,
        replying_instance_id: &str,
    ) -> Result<ReplyRequest> {
        let source_message = runtime
            .state()
            .messages
            .iter()
            .find(|message| message.id == message_id)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("message not found: {message_id}"))?;
        let agent = agents
            .iter()
            .find(|agent| agent.binding.instance_id == replying_instance_id)
            .ok_or_else(|| {
                anyhow::anyhow!("agent not found for instance: {replying_instance_id}")
            })?;
        let responder = agent.binding.responder.as_ref().ok_or_else(|| {
            anyhow::anyhow!("responder binding missing for instance: {replying_instance_id}")
        })?;

        Ok(ReplyRequest {
            source_message_id: source_message.id.clone(),
            source_session_id: source_message.session_id.clone(),
            source_from_instance_id: source_message.from.clone(),
            source_body: source_message.body.clone(),
            replying_instance_id: replying_instance_id.to_owned(),
            replying_agent_name: agent.persona.name.clone(),
            replying_role: agent.persona.role.clone(),
            responder: responder.clone(),
        })
    }
}

fn role_match_score(role: &str, body: &str, route: &MessageRoute) -> f32 {
    let body = body.to_ascii_lowercase();
    let route_bonus: f32 = match route {
        MessageRoute::Direct { .. } => 0.05,
        MessageRoute::Broadcast => 0.0,
        MessageRoute::Channel { .. } => 0.03,
    };

    let base: f32 = match role {
        "planner" => {
            if contains_any(
                &body,
                &["plan", "strategy", "roadmap", "next step", "arrange"],
            ) {
                0.92
            } else {
                0.28
            }
        }
        "worker" => {
            if contains_any(&body, &["implement", "build", "fix", "write", "code"]) {
                0.91
            } else {
                0.34
            }
        }
        "reviewer" => {
            if contains_any(&body, &["review", "check", "risk", "audit", "verify"]) {
                0.90
            } else {
                0.30
            }
        }
        "doctor" => {
            if contains_any(
                &body,
                &["medical", "pain", "symptom", "diagnosis", "health"],
            ) {
                0.95
            } else {
                0.20
            }
        }
        _ => 0.25,
    };

    (base + route_bonus).min(1.0)
}

fn contains_any(body: &str, keywords: &[&str]) -> bool {
    keywords.iter().any(|keyword| body.contains(keyword))
}

#[cfg(test)]
#[path = "../tests/unit/orchestrator.rs"]
mod tests;
