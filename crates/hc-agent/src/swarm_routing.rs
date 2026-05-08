//! P0 rule-based routing tier and task-binding observability (ADR-001 / ADR-004).
//!
//! Correction / planning phrases are loaded via [`crate::swarm_routing_phrase_table::SwarmRoutingPhraseTable`]:
//! default **`{workspace_root}/swarm_routing_phrases.json`**, or **`HC_SWARM_ROUTING_PHRASES_FILE`**.

use hc_protocol::swarm::{
    ROUTING_RULE_VERSION_V1, RoutingDecisionRecord, RoutingTier, SwarmRoutingBindingSnapshot,
    TaskBindingAction, TaskBindingDecisionRecord,
};
use hc_store::task_coordination::implicit_intent_journal_relative;
use hc_trace::TraceEvent;

use crate::swarm_routing_phrase_table::{SwarmRoutingPhraseTable, global_phrase_table};

#[must_use]
fn normalized_lower(input: &str) -> String {
    input.trim().to_lowercase()
}

/// ASCII phrases match on lowercase utf8; needles with non-ASCII match as raw substring.
#[must_use]
fn phrase_list_hit(raw: &str, lower: &str, needles: &[String]) -> bool {
    needles.iter().any(|p| {
        if p.chars().any(|c| !c.is_ascii()) {
            raw.contains(p.as_str())
        } else {
            lower.contains(&p.to_lowercase())
        }
    })
}

fn l3_hard_multi_deliverable(lower: &str) -> bool {
    let outlined_items = lower
        .lines()
        .filter(|l| {
            let t = l.trim_start();
            (t.starts_with("1.")
                || t.starts_with("2.")
                || t.starts_with("- ")
                || t.starts_with("* "))
                && t.len() > 6
        })
        .count();
    let multi_outline = outlined_items >= 2;

    let impl_verify_combo = lower.contains("implement")
        && (lower.contains("verify") || lower.contains("review") || lower.contains("test"));

    multi_outline || impl_verify_combo
}

fn l2_implicit_work_heuristic(lower: &str, raw: &str, keywords: &[String]) -> bool {
    if phrase_list_hit(raw, lower, keywords) {
        return true;
    }
    raw.chars().filter(|c| !c.is_whitespace()).count() > 560
}

/// Classify `RoutingTier` without extra model calls (P0).
#[must_use]
pub fn decide_routing_tier(message_body: &str) -> RoutingDecisionRecord {
    decide_routing_tier_with_phrases(message_body, global_phrase_table())
}

/// Like [`decide_routing_tier`], but uses an explicit phrase table (fixtures / tests).
#[must_use]
pub fn decide_routing_tier_with_phrases(
    message_body: &str,
    phrases: &SwarmRoutingPhraseTable,
) -> RoutingDecisionRecord {
    let raw = message_body.trim();
    let lower = normalized_lower(raw);

    if phrase_list_hit(raw, &lower, &phrases.force_l1) {
        return RoutingDecisionRecord {
            routing_tier: RoutingTier::L1,
            routing_reason: "user_correction_force_l1".to_owned(),
            routing_signals: vec!["force_l1_phrase_hit".to_owned()],
            routing_forced_by_user: true,
            routing_rule_version: ROUTING_RULE_VERSION_V1.to_owned(),
        };
    }

    let user_explicit_l3 = phrase_list_hit(raw, &lower, &phrases.force_l3)
        || phrase_list_hit(raw, &lower, &phrases.l3_collaboration);

    if user_explicit_l3 {
        return RoutingDecisionRecord {
            routing_tier: RoutingTier::L3,
            routing_reason: "explicit_planning_or_collaboration_language".to_owned(),
            routing_signals: vec!["explicit_l3_phrase_hit".to_owned()],
            routing_forced_by_user: true,
            routing_rule_version: ROUTING_RULE_VERSION_V1.to_owned(),
        };
    }

    if l3_hard_multi_deliverable(&lower) {
        return RoutingDecisionRecord {
            routing_tier: RoutingTier::L3,
            routing_reason: "hard_rule_multi_goal_or_impl_verify".to_owned(),
            routing_signals: vec!["multiple_outline_or_impl_verify_combo".to_owned()],
            routing_forced_by_user: false,
            routing_rule_version: ROUTING_RULE_VERSION_V1.to_owned(),
        };
    }

    if l2_implicit_work_heuristic(&lower, raw, &phrases.l2_implicit_keywords) {
        return RoutingDecisionRecord {
            routing_tier: RoutingTier::L2,
            routing_reason: "implicit_work_keywords_or_length".to_owned(),
            routing_signals: vec!["l2_keyword_or_long_input".to_owned()],
            routing_forced_by_user: false,
            routing_rule_version: ROUTING_RULE_VERSION_V1.to_owned(),
        };
    }

    RoutingDecisionRecord {
        routing_tier: RoutingTier::L1,
        routing_reason: "default_conversational_turn".to_owned(),
        routing_signals: vec!["no_promotion_signals".to_owned()],
        routing_forced_by_user: false,
        routing_rule_version: ROUTING_RULE_VERSION_V1.to_owned(),
    }
}

/// Task binding observability aligned with ADR-004 (P0 heuristic).
#[must_use]
pub fn decide_task_binding(
    routing: &RoutingDecisionRecord,
    conversation_active_task_id: Option<&str>,
    fall_back_task_id: Option<&str>,
) -> TaskBindingDecisionRecord {
    match routing.routing_tier {
        RoutingTier::L1 => {
            let effective = conversation_active_task_id.or(fall_back_task_id);
            TaskBindingDecisionRecord {
                active_task_id: effective.map(String::from),
                task_binding_action: TaskBindingAction::NoChange,
                task_binding_reason: "l1_preserve_existing_task_binding_if_any".to_owned(),
                task_binding_signals: Vec::new(),
                task_binding_rule_version: ROUTING_RULE_VERSION_V1.to_owned(),
            }
        }
        RoutingTier::L2 | RoutingTier::L3 => {
            match conversation_active_task_id.or(fall_back_task_id) {
                Some(id) => TaskBindingDecisionRecord {
                    active_task_id: Some(id.to_owned()),
                    task_binding_action: TaskBindingAction::ReuseActiveTask,
                    task_binding_reason: "reuse_task_for_task_scoped_turn".to_owned(),
                    task_binding_signals: Vec::new(),
                    task_binding_rule_version: ROUTING_RULE_VERSION_V1.to_owned(),
                },
                None => TaskBindingDecisionRecord {
                    active_task_id: None,
                    task_binding_action: TaskBindingAction::CreateImplicitTask,
                    task_binding_reason: "no_task_binding_available_for_non_l1_turn".to_owned(),
                    task_binding_signals: Vec::new(),
                    task_binding_rule_version: ROUTING_RULE_VERSION_V1.to_owned(),
                },
            }
        }
    }
}

#[must_use]
pub fn classify_swarm_for_chat_input(
    user_message_body: &str,
    conversation_active_task_id: Option<&str>,
    fall_back_task_id: Option<&str>,
) -> (RoutingDecisionRecord, TaskBindingDecisionRecord) {
    let routing = decide_routing_tier(user_message_body);
    let binding = decide_task_binding(&routing, conversation_active_task_id, fall_back_task_id);
    (routing, binding)
}

/// Same as [`classify_swarm_for_chat_input`], packaged for JSON / fixtures ([`SwarmRoutingBindingSnapshot`]).
#[must_use]
pub fn classify_swarm_snapshot_for_chat_input(
    user_message_body: &str,
    conversation_active_task_id: Option<&str>,
    fall_back_task_id: Option<&str>,
) -> SwarmRoutingBindingSnapshot {
    let (routing, binding) = classify_swarm_for_chat_input(
        user_message_body,
        conversation_active_task_id,
        fall_back_task_id,
    );
    SwarmRoutingBindingSnapshot::new(routing, binding)
}

/// Emits the same routing + task-binding trace as UI runtime paths, for headless HTTP/chat
/// callers that do not have a [`hc_core::MessageRecord`].
#[inline]
pub fn emit_swarm_observability_for_chat_input(
    user_message_body: &str,
    message_id: &str,
    session_id: &str,
    conversation_active_task_id: Option<&str>,
    fall_back_task_id: Option<&str>,
) {
    let snap = classify_swarm_snapshot_for_chat_input(
        user_message_body,
        conversation_active_task_id,
        fall_back_task_id,
    );
    emit_swarm_message_routing(&snap.routing, &snap.task_binding, message_id, session_id);
}

pub fn emit_swarm_message_routing(
    routing: &RoutingDecisionRecord,
    binding: &TaskBindingDecisionRecord,
    message_id: &str,
    session_id: &str,
) {
    let routing_signals_json =
        serde_json::to_string(&routing.routing_signals).unwrap_or_else(|_| "[]".into());
    let binding_signals_json =
        serde_json::to_string(&binding.task_binding_signals).unwrap_or_else(|_| "[]".into());

    hc_trace::emit_trace(
        TraceEvent::info(
            "hc-agent",
            "swarm",
            "routing_and_binding",
            format!(
                "{} | {}",
                routing.routing_tier.as_str(),
                binding.task_binding_action.as_str()
            ),
        )
        .with_field("message_id", message_id)
        .with_field("session_id", session_id)
        .with_field("routing_tier", routing.routing_tier.as_str())
        .with_field("routing_reason", routing.routing_reason.clone())
        .with_field("routing_signals", routing_signals_json)
        .with_field(
            "routing_forced_by_user",
            routing.routing_forced_by_user.to_string(),
        )
        .with_field("routing_rule_version", routing.routing_rule_version.clone())
        .with_field(
            "active_task_id",
            binding
                .active_task_id
                .clone()
                .unwrap_or_else(|| "".to_owned()),
        )
        .with_field("task_binding_action", binding.task_binding_action.as_str())
        .with_field("task_binding_reason", binding.task_binding_reason.clone())
        .with_field("task_binding_signals", binding_signals_json)
        .with_field(
            "task_binding_rule_version",
            binding.task_binding_rule_version.clone(),
        ),
    );
}

/// HTTP **`hc-service` chat**：对 **L2 / L3** 记录与工作台跳过 nomination 对称的退化说明——
/// **`route_agent` + 单次 LLM**，**不产生**基于 message-level nomination / 本路径上的 work-item claim 的 owner。
/// 与 **`planner_only` 工作台物化语义**区分依赖 `routing_tier` + 本事件的 `execution_mode`（ADR-002 可选 **`single_agent_execute`**）。
pub fn emit_http_chat_single_agent_execute_degenerate_trace(
    routing_message_id: &str,
    session_id: &str,
    routing: &RoutingDecisionRecord,
    selected_agent_id: Option<&str>,
) {
    if !matches!(routing.routing_tier, RoutingTier::L2 | RoutingTier::L3) {
        return;
    }
    hc_trace::emit_trace(
        TraceEvent::info(
            "hc-agent",
            "swarm",
            "http_chat_single_agent_execute_degenerate",
            format!(
                "{}: HTTP chat single routed agent generation (task-driven tier without local TaskPlan orchestration)",
                routing.routing_tier.as_str()
            ),
        )
        .with_field("routing_message_id", routing_message_id.to_owned())
        .with_field("session_id", session_id.to_owned())
        .with_field("routing_tier", routing.routing_tier.as_str())
        .with_field("routing_reason", routing.routing_reason.clone())
        .with_field(
            "selected_agent_id",
            selected_agent_id.unwrap_or("").to_owned(),
        )
        .with_field("execution_mode", "single_llm_route_agent"),
    );
}

/// **`hc-service` chat / turn**：L2/L3 且无会话/房间 **task 锚** 时，`decide_task_binding` 产生
/// **`TaskBindingAction::CreateImplicitTask`**。
///
/// **工作台**在已有 `task_scope` 下写 **`coordination/<slug>.implicit-intent.jsonl`**。HTTP 路径由
/// `hc-service` 在同轮分配 **`task.http.implicit.{wall_clock_ms}`**，与本轮 **`routing_message_id`**
///（`{prefix}.{ms}`）一起参与 **ADR-003 implicit dedupe**，并在此 trace 中透出 provisioned **`task_id`**
/// 与相对 journal 路径（若已分配）。
pub fn emit_http_chat_create_implicit_task_binding_trace(
    routing_message_id: &str,
    session_id: &str,
    routing: &RoutingDecisionRecord,
    binding: &TaskBindingDecisionRecord,
    http_implicit_task_id: Option<&str>,
) {
    if binding.task_binding_action != TaskBindingAction::CreateImplicitTask {
        return;
    }
    let journal_rel = http_implicit_task_id
        .map(implicit_intent_journal_relative)
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    hc_trace::emit_trace(
        TraceEvent::info(
            "hc-agent",
            "swarm",
            "http_chat_create_implicit_task_signal",
            format!(
                "{}: create_implicit_task binding (no task anchor on this HTTP turn)",
                routing.routing_tier.as_str()
            ),
        )
        .with_field("routing_message_id", routing_message_id.to_owned())
        .with_field("session_id", session_id.to_owned())
        .with_field("routing_tier", routing.routing_tier.as_str())
        .with_field("task_binding_action", binding.task_binding_action.as_str())
        .with_field("task_binding_reason", binding.task_binding_reason.clone())
        .with_field(
            "http_implicit_task_id",
            http_implicit_task_id.unwrap_or("").to_owned(),
        )
        .with_field("implicit_intent_journal_relative", journal_rel),
    );
}

#[cfg(test)]
mod routing_tests {
    use super::*;
    use crate::SwarmRoutingPhraseTable;
    use hc_protocol::swarm::{
        ROUTING_RULE_VERSION_V1, RoutingTier, SwarmRoutingBindingSnapshot, TaskBindingAction,
    };

    #[test]
    fn custom_phrase_table_can_trigger_l3() {
        let mut t = SwarmRoutingPhraseTable::builtins();
        t.force_l1.clear();
        t.force_l3 = vec!["zzzunique_plan_token".into()];
        t.l3_collaboration.clear();
        t.l2_implicit_keywords.clear();
        let r = decide_routing_tier_with_phrases("short text with zzzunique_plan_token please", &t);
        assert_eq!(r.routing_tier, RoutingTier::L3);
        assert!(r.routing_forced_by_user);
    }

    #[test]
    fn snapshot_matches_tuple_classification_and_roundtrips_json() {
        let tuple = classify_swarm_for_chat_input("plan this please", Some("t1"), None);
        let snap = classify_swarm_snapshot_for_chat_input("plan this please", Some("t1"), None);
        assert_eq!(tuple.0, snap.routing);
        assert_eq!(tuple.1, snap.task_binding);
        let json = serde_json::to_string(&snap).expect("serde");
        let back: SwarmRoutingBindingSnapshot = serde_json::from_str(&json).expect("de");
        assert_eq!(back, snap);
    }

    #[test]
    fn binding_prefers_conversation_active_over_workspace_hint_for_l3() {
        let routing = decide_routing_tier("please plan this with steps.");
        assert_eq!(routing.routing_tier, RoutingTier::L3);
        let binding = decide_task_binding(
            &routing,
            Some("session.bound.task"),
            Some("workspace.default.task"),
        );
        assert_eq!(
            binding.active_task_id.as_deref(),
            Some("session.bound.task")
        );
        assert_eq!(
            binding.task_binding_action,
            TaskBindingAction::ReuseActiveTask
        );
        assert_eq!(binding.task_binding_rule_version, ROUTING_RULE_VERSION_V1);
    }

    #[test]
    fn default_plain_query_is_l1() {
        let r = decide_routing_tier("What is Borrow in Rust?");
        assert_eq!(r.routing_tier, RoutingTier::L1);
        assert!(!r.routing_forced_by_user);
    }

    #[test]
    fn plan_this_is_l3() {
        let r = decide_routing_tier("Please plan this with steps.");
        assert_eq!(r.routing_tier, RoutingTier::L3);
        assert!(r.routing_forced_by_user);
        assert_eq!(r.routing_rule_version, ROUTING_RULE_VERSION_V1);
    }

    #[test]
    fn force_l1_overrides_l3_phrases() {
        let r = decide_routing_tier("别拆任务，直接回答 plan this 是啥");
        assert_eq!(r.routing_tier, RoutingTier::L1);
        assert!(r.routing_forced_by_user);
    }
}
