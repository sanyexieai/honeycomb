use anyhow::Result;
use hc_core::{
    MessageKind, MessageRecord, MessageRoute, ParticipationClaim, RuntimeCommand,
    RuntimeCommandResult, RuntimeSupervisor, SpeakingGrant,
};
use hc_responder::{ReplyRequest, ResponderBackend};
use serde::{Deserialize, Serialize};

use crate::{AgentPlan, IncubationReport, MaterializedAgent, TaskRequest};

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
    ) -> Result<Option<SpeakingGrant>> {
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
        Ok(grant)
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
            .ok_or_else(|| anyhow::anyhow!("agent not found for instance: {}", grant.instance_id))?;
        let responder = agent
            .binding
            .responder
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("responder binding missing for instance: {}", grant.instance_id))?;

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
            .ok_or_else(|| anyhow::anyhow!("agent not found for instance: {replying_instance_id}"))?;
        let responder = agent
            .binding
            .responder
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("responder binding missing for instance: {replying_instance_id}"))?;

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
            if contains_any(&body, &["plan", "strategy", "roadmap", "next step", "arrange"]) {
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
            if contains_any(&body, &["medical", "pain", "symptom", "diagnosis", "health"]) {
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
mod tests {
    use super::*;
    use hc_responder::ReplyResponse;
    use crate::{TaskRequest, bootstrap_task, materialize_plan};

    #[derive(Debug, Clone, Default)]
    struct EchoBackend;

    impl ResponderBackend for EchoBackend {
        fn generate_reply(&self, request: &ReplyRequest) -> Result<ReplyResponse> {
            Ok(ReplyResponse::new(format!("echo:{}", request.source_body)))
        }
    }

    #[test]
    fn orchestrator_suggests_claims_for_matching_roles() {
        let mut runtime = RuntimeSupervisor::new();
        let session = runtime.create_session("demo");
        let task = TaskRequest::new("task.demo", "Demo Task", "Build a demo");
        let plan = bootstrap_task(&task);
        let agents = materialize_plan(&mut runtime, &session.id, &plan)
            .expect("materialization should succeed");

        let message = runtime
            .post_message(
                &session.id,
                &agents[0].binding.instance_id,
                MessageRoute::Broadcast,
                hc_core::MessageKind::Chat,
                "please review and verify the risks in this implementation",
                None,
            )
            .expect("message should succeed");

        let orchestrator = AgentOrchestrator::new();
        let claims = orchestrator
            .suggest_claims_for_message(&runtime, &agents, &message, 100)
            .expect("claim suggestion should succeed");

        assert!(!claims.is_empty());
        let reviewer_claim = claims
            .iter()
            .find(|claim| {
                claim.instance_id
                    == agents
                        .iter()
                        .find(|agent| agent.seed.role == "reviewer")
                        .expect("reviewer should exist")
                        .binding
                        .instance_id
            })
            .expect("reviewer should claim");
        assert!(reviewer_claim.score >= 0.90);
    }

    #[test]
    fn orchestrator_can_complete_nomination_cycle() {
        let mut runtime = RuntimeSupervisor::new();
        let session = runtime.create_session("demo");
        let task = TaskRequest::new("task.demo", "Demo Task", "Build a demo");
        let plan = bootstrap_task(&task);
        let agents = materialize_plan(&mut runtime, &session.id, &plan)
            .expect("materialization should succeed");

        let message = runtime
            .post_message(
                &session.id,
                &agents[0].binding.instance_id,
                MessageRoute::Broadcast,
                hc_core::MessageKind::Chat,
                "please review the risks in this implementation plan",
                None,
            )
            .expect("message should succeed");

        let orchestrator = AgentOrchestrator::new();
        let suggested_claims = orchestrator
            .suggest_claims_for_message(&runtime, &agents, &message, 100)
            .expect("claim suggestion should succeed");
        let grant = orchestrator
            .run_nomination_cycle(&mut runtime, &agents, &message, 100)
            .expect("nomination cycle should succeed")
            .expect("winner should exist");

        let expected_winner = suggested_claims
            .iter()
            .filter(|claim| claim.score >= 0.85)
            .max_by(|left, right| left.score.total_cmp(&right.score))
            .expect("at least one high-confidence claim should exist");
        assert_eq!(grant.instance_id, expected_winner.instance_id);
    }

    #[test]
    fn orchestrator_can_generate_and_post_reply_for_granted_agent() {
        let mut runtime = RuntimeSupervisor::new();
        let session = runtime.create_session("demo");
        let task = TaskRequest::new("task.demo", "Demo Task", "Build a demo");
        let plan = bootstrap_task(&task);
        let agents = materialize_plan(&mut runtime, &session.id, &plan)
            .expect("materialization should succeed");
        let message = runtime
            .post_message(
                &session.id,
                &agents[0].binding.instance_id,
                MessageRoute::Broadcast,
                hc_core::MessageKind::Chat,
                "please review the risks in this implementation",
                None,
            )
            .expect("message should succeed");

        let orchestrator = AgentOrchestrator::new();
        let grant = orchestrator
            .run_nomination_cycle(&mut runtime, &agents, &message, 100)
            .expect("nomination cycle should succeed")
            .expect("winner should exist");

        let reply = orchestrator
            .generate_and_post_reply(&EchoBackend, &mut runtime, &agents, &grant)
            .expect("reply generation should succeed");

        assert_eq!(reply.reply_to.as_deref(), Some(message.id.as_str()));
        assert!(reply.body.starts_with("echo:"));
        match &reply.route {
            MessageRoute::Direct { to } => assert_eq!(to, &message.from),
            route => panic!("expected direct reply route, got {route:?}"),
        }
    }

    #[test]
    fn orchestrator_can_generate_and_post_direct_reply() {
        let mut runtime = RuntimeSupervisor::new();
        let session = runtime.create_session("demo");
        let task = TaskRequest::new("task.demo", "Demo Task", "Build a demo");
        let plan = bootstrap_task(&task);
        let agents = materialize_plan(&mut runtime, &session.id, &plan)
            .expect("materialization should succeed");

        let source = &agents[0];
        let replier = agents
            .iter()
            .find(|agent| agent.seed.role == "worker")
            .expect("worker should exist");

        let message = runtime
            .post_message(
                &session.id,
                &source.binding.instance_id,
                MessageRoute::Direct {
                    to: replier.binding.instance_id.clone(),
                },
                hc_core::MessageKind::Chat,
                "implement this feature",
                None,
            )
            .expect("message should succeed");

        let orchestrator = AgentOrchestrator::new();
        let reply = orchestrator
            .generate_and_post_direct_reply(
                &EchoBackend,
                &mut runtime,
                &agents,
                &message.id,
                &replier.binding.instance_id,
            )
            .expect("direct reply generation should succeed");

        assert_eq!(reply.reply_to.as_deref(), Some(message.id.as_str()));
        match &reply.route {
            MessageRoute::Direct { to } => assert_eq!(to, &message.from),
            route => panic!("expected direct reply route, got {route:?}"),
        }
    }
}
