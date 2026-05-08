use super::*;
use crate::{TaskRequest, TaskBootstrapPreset, bootstrap_task_with_preset, materialize_plan};
use hc_protocol::swarm::RoutingTier;
use hc_responder::ReplyResponse;

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
    let plan = bootstrap_task_with_preset(&task, TaskBootstrapPreset::ThreeRolesDemo);
    let outcome =
        materialize_plan(&mut runtime, &session.id, &plan).expect("materialization should succeed");
    let agents = outcome.agents;

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
    let plan = bootstrap_task_with_preset(&task, TaskBootstrapPreset::ThreeRolesDemo);
    let outcome =
        materialize_plan(&mut runtime, &session.id, &plan).expect("materialization should succeed");
    let agents = outcome.agents;

    let message = runtime
        .post_message(
            &session.id,
            &agents[0].binding.instance_id,
            MessageRoute::Broadcast,
            hc_core::MessageKind::Chat,
            "just answer directly: mention two checklist items worth attention when reviewing a small API change.",
            None,
        )
        .expect("message should succeed");

    let orchestrator = AgentOrchestrator::new();
    let suggested_claims = orchestrator
        .suggest_claims_for_message(&runtime, &agents, &message, 100)
        .expect("claim suggestion should succeed");
    let grant = orchestrator
        .run_nomination_cycle(&mut runtime, &agents, &message, 100, None, None)
        .expect("nomination cycle should succeed")
        .grant
        .expect("winner should exist");

    let expected_winner = suggested_claims
        .iter()
        .filter(|claim| claim.score >= 0.85)
        .max_by(|left, right| left.score.total_cmp(&right.score))
        .expect("at least one high-confidence claim should exist");
    assert_eq!(grant.instance_id, expected_winner.instance_id);
}

#[test]
fn orchestrator_nomination_cycle_attaches_swarm_for_chat() {
    let mut runtime = RuntimeSupervisor::new();
    let session = runtime.create_session("demo");
    let task = TaskRequest::new("task.demo", "Demo Task", "Build a demo");
    let plan = bootstrap_task_with_preset(&task, TaskBootstrapPreset::ThreeRolesDemo);
    let outcome =
        materialize_plan(&mut runtime, &session.id, &plan).expect("materialization should succeed");
    let agents = outcome.agents;

    let message = runtime
        .post_message(
            &session.id,
            &agents[0].binding.instance_id,
            MessageRoute::Broadcast,
            hc_core::MessageKind::Chat,
            "say hello",
            None,
        )
        .expect("message should succeed");

    let orchestrator = AgentOrchestrator::new();
    let outcome = orchestrator
        .run_nomination_cycle(&mut runtime, &agents, &message, 100, None, None)
        .expect("nomination cycle should succeed");
    assert!(
        outcome.swarm.is_some(),
        "chat messages should carry swarm classification"
    );
    assert!(outcome.swarm.as_ref().unwrap().routing.routing_reason.len() > 2);
}

#[test]
fn orchestrator_skips_message_nomination_for_l3_routing() {
    let mut runtime = RuntimeSupervisor::new();
    let session = runtime.create_session("demo");
    let task = TaskRequest::new("task.demo", "Demo Task", "Build a demo");
    let plan = bootstrap_task_with_preset(&task, TaskBootstrapPreset::ThreeRolesDemo);
    let outcome =
        materialize_plan(&mut runtime, &session.id, &plan).expect("materialization should succeed");
    let agents = outcome.agents;

    let message = runtime
        .post_message(
            &session.id,
            &agents[0].binding.instance_id,
            MessageRoute::Broadcast,
            hc_core::MessageKind::Chat,
            "Please plan this with steps for rollout.",
            None,
        )
        .expect("message should succeed");

    let orchestrator = AgentOrchestrator::new();
    let outcome = orchestrator
        .run_nomination_cycle(&mut runtime, &agents, &message, 100, None, None)
        .expect("nomination cycle should succeed");
    assert!(
        outcome.grant.is_none(),
        "L3 routing should bypass message-level speaking grant selection"
    );
    let swarm = outcome.swarm.expect("chat should classify");
    assert_eq!(swarm.routing.routing_tier, RoutingTier::L3);
}

#[test]
fn orchestrator_skips_message_nomination_for_l2_routing() {
    let mut runtime = RuntimeSupervisor::new();
    let session = runtime.create_session("demo");
    let task = TaskRequest::new("task.demo", "Demo Task", "Build a demo");
    let plan = bootstrap_task_with_preset(&task, TaskBootstrapPreset::ThreeRolesDemo);
    let outcome =
        materialize_plan(&mut runtime, &session.id, &plan).expect("materialization should succeed");
    let agents = outcome.agents;

    let message = runtime
        .post_message(
            &session.id,
            &agents[0].binding.instance_id,
            MessageRoute::Broadcast,
            hc_core::MessageKind::Chat,
            "Please refactor crate::net handshake for clarity.",
            None,
        )
        .expect("message should succeed");

    let orchestrator = AgentOrchestrator::new();
    let outcome = orchestrator
        .run_nomination_cycle(&mut runtime, &agents, &message, 100, None, None)
        .expect("nomination cycle should succeed");
    assert!(
        outcome.grant.is_none(),
        "L2 routing should bypass message-level speaking grant selection"
    );
    let swarm = outcome.swarm.expect("chat should classify");
    assert_eq!(swarm.routing.routing_tier, RoutingTier::L2);
}

#[test]
fn orchestrator_can_generate_and_post_reply_for_granted_agent() {
    let mut runtime = RuntimeSupervisor::new();
    let session = runtime.create_session("demo");
    let task = TaskRequest::new("task.demo", "Demo Task", "Build a demo");
    let plan = bootstrap_task_with_preset(&task, TaskBootstrapPreset::ThreeRolesDemo);
    let outcome =
        materialize_plan(&mut runtime, &session.id, &plan).expect("materialization should succeed");
    let agents = outcome.agents;
    let message = runtime
        .post_message(
            &session.id,
            &agents[0].binding.instance_id,
            MessageRoute::Broadcast,
            hc_core::MessageKind::Chat,
            "don't split this — list two pitfalls to watch for when reviewing a tiny config tweak.",
            None,
        )
        .expect("message should succeed");

    let orchestrator = AgentOrchestrator::new();
    let grant = orchestrator
        .run_nomination_cycle(&mut runtime, &agents, &message, 100, None, None)
        .expect("nomination cycle should succeed")
        .grant
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
    let plan = bootstrap_task_with_preset(&task, TaskBootstrapPreset::ThreeRolesDemo);
    let outcome =
        materialize_plan(&mut runtime, &session.id, &plan).expect("materialization should succeed");
    let agents = outcome.agents;

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
