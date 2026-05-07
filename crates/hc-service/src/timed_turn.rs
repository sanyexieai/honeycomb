//! Timed reminders and per-interval countdown sequences shared by API and CLI.
//!
//! Routing metadata comes from each tenant user's `routing/tool-routing-tags.md` frontmatter,
//! merged with [`ToolRoutingTags`] in [`crate::tool_turn`].

use std::thread;
use std::time::Duration;
use std::collections::HashSet;

use anyhow::Result;
use hc_agent::phrase_match_score;
use hc_context::runtime::{DEFAULT_TENANT_ID, DEFAULT_USER_ID, default_session_id};
use hc_conversation::{ConversationRepository, PendingFollowUp};
use hc_intent::{IntentResolution, ids as intent_ids};
use hc_protocol::{ApiChatMessage, ApiMessageRole, ApiNamespace, ChatRequest, ChatResponse};
use hc_scheduler::{
    ScheduleKind, ScheduleRepository, ScheduleSpec, ScheduledTarget, ScheduledTargetKind,
    ScheduledTask, now_unix,
};
use hc_store::store::WorkspaceNamespace;
use serde::Deserialize;

use crate::{
    ServiceConfig,
    tool_turn::{ToolRoutingTags, load_tool_routing_tags, request_input, request_namespace},
};

#[derive(Debug, Clone)]
struct TimedRunSpec {
    id: String,
    due_at_unix: u64,
    draft_message: String,
    notes: String,
    payload: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Clone)]
struct TimedTaskSpec {
    agent_id: String,
    trigger: String,
    room_id: Option<String>,
    runs: Vec<TimedRunSpec>,
}

#[derive(Debug, Clone, Copy)]
pub enum TimedDeliverMode {
    /// CLI REPL: synchronous tick printing and optional reminder stdout when firing.
    Interactive,
    /// HTTP API: schedule repository writes only; no blocking tick loop or stderr printing.
    Headless,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct TimedSequenceRule {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub hints: Vec<String>,
    #[serde(default)]
    pub direction: String,
    #[serde(default)]
    pub default_end: Option<i64>,
    #[serde(default = "default_timed_sequence_interval_seconds")]
    pub interval_seconds: u64,
    #[serde(default = "default_timed_sequence_max_items")]
    pub max_items: usize,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub trigger: Option<String>,
    #[serde(default)]
    pub scheduled_reply: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ReminderRule {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub hints: Vec<String>,
    #[serde(default = "default_reminder_delay_seconds")]
    pub default_delay_seconds: u64,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub trigger: Option<String>,
    #[serde(default)]
    pub scheduled_reply: Option<String>,
    #[serde(default)]
    pub due_reply: Option<String>,
}

impl Default for ReminderRule {
    fn default() -> Self {
        Self {
            id: String::new(),
            hints: Vec::new(),
            default_delay_seconds: default_reminder_delay_seconds(),
            agent_id: None,
            trigger: None,
            scheduled_reply: None,
            due_reply: None,
        }
    }
}

fn default_reminder_delay_seconds() -> u64 {
    60
}

fn default_timed_sequence_interval_seconds() -> u64 {
    1
}

fn default_timed_sequence_max_items() -> usize {
    120
}

pub(crate) fn builtin_timed_sequence_rules() -> Vec<TimedSequenceRule> {
    vec![TimedSequenceRule {
        id: "builtin.countdown.cn".to_owned(),
        hints: vec![
            "倒数".to_owned(),
            "倒计时".to_owned(),
            "countdown".to_owned(),
            "每秒".to_owned(),
        ],
        direction: "countdown".to_owned(),
        default_end: Some(1),
        interval_seconds: default_timed_sequence_interval_seconds(),
        max_items: default_timed_sequence_max_items(),
        agent_id: None,
        trigger: None,
        scheduled_reply: Some("好，按间隔播报。".to_owned()),
    }]
}

pub(crate) fn builtin_reminder_rules() -> Vec<ReminderRule> {
    vec![ReminderRule {
        id: "builtin.reminder.cn".to_owned(),
        hints: vec![
            "提醒".to_owned(),
            "叫我".to_owned(),
            "稍后".to_owned(),
            "等会".to_owned(),
            "remind".to_owned(),
        ],
        default_delay_seconds: default_reminder_delay_seconds(),
        agent_id: None,
        trigger: None,
        scheduled_reply: Some("好，到时间我会提醒您。".to_owned()),
        due_reply: Some("到时间了。".to_owned()),
    }]
}

fn text_matches_any(text: &str, selectors: &[String]) -> bool {
    selectors
        .iter()
        .any(|selector| phrase_match_score(text, selector) > 0)
}

/// Ordered assistant text segments for streaming transports (SSE / WebSocket): one delta per entry.
#[derive(Debug, Clone)]
pub struct TimedStreamPlan {
    pub chunks: Vec<String>,
    pub pause_between_chunks_ms: u64,
    pub final_response: ChatResponse,
}

/// Same routing as [`try_handle_timed_chat_turn`], but returns multiple chunks for stream emission.
pub fn try_timed_stream_plan(
    config: &ServiceConfig,
    request: &ChatRequest,
    intent: &IntentResolution,
    history_for_match: &[ApiChatMessage],
) -> Result<Option<TimedStreamPlan>> {
    let input = request_input(request)?;
    let namespace = request_namespace(request);
    let mut routing =
        load_tool_routing_tags(config, &namespace).unwrap_or_else(|_| ToolRoutingTags::default());
    routing.ensure_builtin_timed_sequences();

    if let Some(text) = handle_reminder_turn(config, request, &routing, TimedDeliverMode::Headless)?
    {
        let final_response = chat_response_simple(request, &namespace, text.clone());
        return Ok(Some(TimedStreamPlan {
            chunks: vec![text],
            pause_between_chunks_ms: 0,
            final_response,
        }));
    }

    if let Some((rule, values)) =
        timed_sequence_match_for_turn(&routing, &input, history_for_match, intent)
    {
        let Some(final_response) = execute_timed_sequence(
            config,
            request,
            &namespace,
            rule.clone(),
            values.clone(),
            TimedDeliverMode::Headless,
        )?
        else {
            return Ok(None);
        };

        let mut chunks: Vec<String> = values.iter().map(|v| v.to_string()).collect();
        let ack = rule
            .scheduled_reply
            .clone()
            .unwrap_or_else(|| "scheduled timed sequence".to_owned());
        chunks.push(ack);

        let pause_between_chunks_ms = rule.interval_seconds.saturating_mul(1000);
        return Ok(Some(TimedStreamPlan {
            chunks,
            pause_between_chunks_ms,
            final_response,
        }));
    }

    Ok(None)
}

/// Intent-aware timed sequence / reminder handling. Call after pending confirmation, before MCP.
pub fn try_handle_timed_chat_turn(
    config: &ServiceConfig,
    request: &ChatRequest,
    intent: &IntentResolution,
    deliver: TimedDeliverMode,
    history_for_match: &[ApiChatMessage],
) -> Result<Option<ChatResponse>> {
    let input = request_input(request)?;
    let namespace = request_namespace(request);
    let mut routing =
        load_tool_routing_tags(config, &namespace).unwrap_or_else(|_| ToolRoutingTags::default());
    routing.ensure_builtin_timed_sequences();

    if let Some(text) = handle_reminder_turn(config, request, &routing, deliver)? {
        return Ok(Some(chat_response_simple(request, &namespace, text)));
    }

    if let Some((rule, values)) =
        timed_sequence_match_for_turn(&routing, &input, history_for_match, intent)
    {
        return execute_timed_sequence(config, request, &namespace, rule, values, deliver);
    }

    Ok(None)
}

fn chat_response_simple(
    request: &ChatRequest,
    namespace: &ApiNamespace,
    content: String,
) -> ChatResponse {
    ChatResponse {
        message: ApiChatMessage {
            role: ApiMessageRole::Assistant,
            content,
            name: None,
        },
        model: request.model.clone().unwrap_or_default(),
        provider: request.provider.clone().unwrap_or_default(),
        tenant_id: Some(namespace.tenant_id.clone()),
        user_id: Some(namespace.user_id.clone()),
        session_id: request.session_id.clone().or_else(|| {
            Some(default_session_id(
                request.tenant_id.as_deref().unwrap_or(DEFAULT_TENANT_ID),
                request.user_id.as_deref().unwrap_or(DEFAULT_USER_ID),
            ))
        }),
        room_id: request.room_id.clone(),
        selected_agent_id: request.agent_id.clone(),
        selected_domain_id: request.domain_id.clone(),
        recalled_memories: Vec::new(),
        synthesized_prompt_asset_count: 0,
        room_capabilities_used: Vec::new(),
        room_tools_used: Vec::new(),
        room_skills_used: Vec::new(),
        behavior_pattern_used: None,
        decision_reasoning: None,
        decision_confidence: None,
    }
}

fn handle_reminder_turn(
    config: &ServiceConfig,
    request: &ChatRequest,
    routing: &ToolRoutingTags,
    deliver: TimedDeliverMode,
) -> Result<Option<String>> {
    let input = request_input(request)?;
    let Some((rule, delay_seconds)) = reminder_for_turn(&input, routing) else {
        return Ok(None);
    };
    let namespace_api = request_namespace(request);
    let namespace = WorkspaceNamespace::new(
        namespace_api.tenant_id.clone(),
        namespace_api.user_id.clone(),
    );
    let repository =
        ConversationRepository::with_namespace(config.workspace_root.clone(), namespace.clone());
    let now = now_unix();
    let reminder_prefix = if rule.id.trim().is_empty() {
        "reminder"
    } else {
        rule.id.trim()
    };
    let reminder_id = format!("{reminder_prefix}.{now}");
    let trigger = rule
        .trigger
        .clone()
        .unwrap_or_else(|| "reminder.due".to_owned());
    let agent_id = rule
        .agent_id
        .clone()
        .unwrap_or_else(|| "agent.system.reminder".to_owned());
    let due_reply = rule
        .due_reply
        .clone()
        .unwrap_or_else(|| "到时间了。".to_owned());
    let mut payload = serde_json::Map::new();
    payload.insert(
        "source_turn".to_owned(),
        serde_json::Value::String(input.clone()),
    );
    let spec = TimedTaskSpec {
        agent_id,
        trigger,
        room_id: request.session_id.clone(),
        runs: vec![TimedRunSpec {
            id: reminder_id.clone(),
            due_at_unix: now.saturating_add(delay_seconds),
            draft_message: due_reply,
            notes: format!("Reminder due in {delay_seconds} seconds."),
            payload,
        }],
    };
    let followup_ids = persist_timed_task_runs(config, &namespace, &repository, spec)?;

    if matches!(deliver, TimedDeliverMode::Interactive) {
        spawn_interactive_followup_delivery_worker(config, &namespace_api, followup_ids);
    }

    Ok(Some(
        rule.scheduled_reply
            .clone()
            .unwrap_or_else(|| "好，到时间我会提醒您。".to_owned()),
    ))
}

fn spawn_interactive_followup_delivery_worker(
    config: &ServiceConfig,
    namespace: &ApiNamespace,
    followup_ids: Vec<String>,
) {
    let config = config.clone();
    let namespace = namespace.clone();
    thread::spawn(move || {
        if let Err(error) = deliver_followups_interactive(&config, &namespace, &followup_ids) {
            eprintln!("warning> timed followup delivery failed: {error}");
        }
    });
}

fn deliver_followups_interactive(
    config: &ServiceConfig,
    namespace: &ApiNamespace,
    followup_ids: &[String],
) -> Result<()> {
    struct StdoutFollowUpSink;
    impl crate::scheduler::FollowUpMessageSink for StdoutFollowUpSink {
        fn on_fired_followup_message(
            &mut self,
            message: &crate::scheduler::FiredFollowUpMessage,
        ) {
            println!("assistant> {}", message.message);
        }
    }

    let mut pending: HashSet<String> = followup_ids.iter().cloned().collect();
    let mut sink = StdoutFollowUpSink;
    while !pending.is_empty() {
        let report = crate::scheduler::dispatch_due_scheduled_runs(
            config,
            namespace.clone(),
            Some(now_unix()),
        )?;
        let delivered_ids = crate::scheduler::dispatch_fired_followup_messages_from_receipts(
            config,
            namespace.clone(),
            &report.receipts,
            &mut sink,
        )?;
        for id in delivered_ids {
            pending.remove(&id);
        }
        if !pending.is_empty() {
            thread::sleep(Duration::from_millis(200));
        }
    }
    Ok(())
}

fn persist_timed_task_runs(
    config: &ServiceConfig,
    namespace: &WorkspaceNamespace,
    repository: &ConversationRepository,
    spec: TimedTaskSpec,
) -> Result<Vec<String>> {
    let schedule_repository =
        ScheduleRepository::with_namespace(config.workspace_root.clone(), namespace.clone());
    let mut followup_ids = Vec::new();
    for run in spec.runs {
        let mut followup = PendingFollowUp::new(
            spec.agent_id.clone(),
            spec.trigger.clone(),
            run.due_at_unix,
        );
        followup.id = run.id;
        followup.room_id = spec.room_id.clone();
        followup.payload = run.payload;
        followup.payload.insert(
            "draft_message".to_owned(),
            serde_json::Value::String(run.draft_message),
        );
        followup.notes = run.notes;
        repository.write_followup(&followup)?;
        let mut target_args = serde_json::Map::new();
        target_args.insert(
            "followup_id".to_owned(),
            serde_json::Value::String(followup.id.clone()),
        );
        let mut schedule = ScheduledTask::new(
            format!("timed.followup.{}", followup.id),
            format!("Timed followup {}", followup.id),
            ScheduleSpec {
                kind: ScheduleKind::Once,
                run_at_unix: Some(followup.due_at_unix),
                interval_seconds: None,
            },
            ScheduledTarget {
                kind: ScheduledTargetKind::Event,
                r#ref: "timed.followup".to_owned(),
                action: Some("timed.followup.fire".to_owned()),
                args: target_args,
            },
        );
        schedule.tags = vec!["scheduled".to_owned(), "timed".to_owned(), "followup".to_owned()];
        schedule.notes = "Mirrored from timed_turn followup queue.".to_owned();
        schedule_repository.write_schedule(&schedule)?;
        followup_ids.push(followup.id);
    }
    Ok(followup_ids)
}

fn reminder_for_turn(user_turn: &str, routing: &ToolRoutingTags) -> Option<(ReminderRule, u64)> {
    routing.reminder_rules.iter().find_map(|rule| {
        if !text_matches_any(user_turn, &rule.hints) {
            return None;
        }
        let delay_seconds = reminder_delay_seconds(user_turn, rule.default_delay_seconds)?;
        Some((rule.clone(), delay_seconds))
    })
}

pub fn reminder_delay_seconds(text: &str, default_delay_seconds: u64) -> Option<u64> {
    let numbers = extract_i64_numbers(text);
    let value = numbers
        .first()
        .copied()
        .filter(|value| *value > 0)
        .map(|value| value as u64);
    let unit_seconds = if contains_any(text, &["毫秒", "ms"]) {
        0
    } else if contains_any(text, &["小时", "钟头", "hour", "hours", "h"]) {
        60 * 60
    } else if contains_any(text, &["分钟", "分", "minute", "minutes", "min"]) {
        60
    } else if contains_any(text, &["秒", "second", "seconds", "sec", "s"]) {
        1
    } else {
        return Some(default_delay_seconds);
    };
    Some(value.unwrap_or(1).saturating_mul(unit_seconds).max(1))
}

fn contains_any(text: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| text.contains(needle))
}

fn execute_timed_sequence(
    config: &ServiceConfig,
    request: &ChatRequest,
    namespace: &ApiNamespace,
    rule: TimedSequenceRule,
    values: Vec<i64>,
    deliver: TimedDeliverMode,
) -> Result<Option<ChatResponse>> {
    let ws = WorkspaceNamespace::new(namespace.tenant_id.clone(), namespace.user_id.clone());
    let repository = ConversationRepository::with_namespace(config.workspace_root.clone(), ws.clone());
    let now = now_unix();
    let sequence_prefix = if rule.id.trim().is_empty() {
        "timed-sequence"
    } else {
        rule.id.trim()
    };
    let sequence_id = format!("{sequence_prefix}.{now}");
    let trigger = rule
        .trigger
        .clone()
        .unwrap_or_else(|| "timed_sequence.tick".to_owned());
    let agent_id = rule
        .agent_id
        .clone()
        .unwrap_or_else(|| "agent.system.timer".to_owned());

    let mut runs = Vec::with_capacity(values.len());
    for (index, value) in values.iter().enumerate() {
        let mut payload = serde_json::Map::new();
        payload.insert(
            "sequence_id".to_owned(),
            serde_json::Value::String(sequence_id.clone()),
        );
        payload.insert("index".to_owned(), serde_json::json!(index));
        runs.push(TimedRunSpec {
            id: format!("{sequence_id}.{index}"),
            due_at_unix: now.saturating_add(rule.interval_seconds.saturating_mul(index as u64)),
            draft_message: value.to_string(),
            notes: format!("Timed sequence tick {index}: {value}"),
            payload,
        });
    }
    let followup_ids = persist_timed_task_runs(
        config,
        &ws,
        &repository,
        TimedTaskSpec {
            agent_id,
            trigger,
            room_id: request.session_id.clone(),
            runs,
        },
    )?;

    let ack = rule
        .scheduled_reply
        .clone()
        .unwrap_or_else(|| "scheduled timed sequence".to_owned());

    if matches!(deliver, TimedDeliverMode::Interactive) {
        deliver_followups_interactive(config, namespace, &followup_ids)?;
    }

    Ok(Some(chat_response_simple(request, namespace, ack)))
}

pub(crate) fn timed_sequence_match_for_turn(
    routing: &ToolRoutingTags,
    user_turn: &str,
    history: &[ApiChatMessage],
    intent: &IntentResolution,
) -> Option<(TimedSequenceRule, Vec<i64>)> {
    timed_sequence_for_turn_with_history(routing, user_turn, history)
        .or_else(|| timed_sequence_intent_fallback(routing, user_turn, intent))
}

fn timed_sequence_for_turn_with_history(
    routing: &ToolRoutingTags,
    user_turn: &str,
    history: &[ApiChatMessage],
) -> Option<(TimedSequenceRule, Vec<i64>)> {
    if let Some(sequence) = timed_sequence_for_turn(routing, user_turn) {
        return Some(sequence);
    }
    let current_matches_timing = routing
        .timed_sequence_rules
        .iter()
        .any(|rule| text_matches_any(user_turn, &rule.hints));
    if !current_matches_timing {
        return None;
    }
    let previous_user_turn = history
        .iter()
        .rev()
        .find(|message| message.role == ApiMessageRole::User && !message.content.trim().is_empty())
        .map(|message| message.content.trim())?;
    timed_sequence_for_turn(routing, &format!("{previous_user_turn} {user_turn}"))
}

fn timed_sequence_intent_fallback(
    routing: &ToolRoutingTags,
    user_turn: &str,
    intent: &IntentResolution,
) -> Option<(TimedSequenceRule, Vec<i64>)> {
    if intent.primary_intent != intent_ids::INTERACTION_TIMED_EMIT {
        return None;
    }
    let rule = routing
        .timed_sequence_rules
        .iter()
        .find(|r| r.id == "builtin.countdown.cn")
        .or_else(|| {
            routing
                .timed_sequence_rules
                .iter()
                .find(|r| r.direction == "countdown")
        })?;

    let numbers = extract_i64_numbers(user_turn);
    let start = *numbers.first()?;
    let end = timed_sequence_end(user_turn, start, &numbers, rule);
    let values = build_timed_sequence_values(start, end, rule)?;
    Some((rule.clone(), values))
}

fn timed_sequence_for_turn(
    routing: &ToolRoutingTags,
    user_turn: &str,
) -> Option<(TimedSequenceRule, Vec<i64>)> {
    routing.timed_sequence_rules.iter().find_map(|rule| {
        if !text_matches_any(user_turn, &rule.hints) {
            return None;
        }
        let numbers = extract_i64_numbers(user_turn);
        let start = *numbers.first()?;
        let end = timed_sequence_end(user_turn, start, &numbers, rule);
        let values = build_timed_sequence_values(start, end, rule)?;
        Some((rule.clone(), values))
    })
}

pub fn timed_sequence_end(
    user_turn: &str,
    start: i64,
    numbers: &[i64],
    rule: &TimedSequenceRule,
) -> i64 {
    if let Some(end) = numbers.get(1).copied() {
        return end;
    }
    if is_count_quantity_turn(user_turn) && rule.direction == "countdown" {
        return 1;
    }
    rule.default_end.unwrap_or(start)
}

fn is_count_quantity_turn(text: &str) -> bool {
    (text.contains("个数") || text.contains("个数字") || text.contains("个"))
        && !text.contains("到")
}

fn build_timed_sequence_values(start: i64, end: i64, rule: &TimedSequenceRule) -> Option<Vec<i64>> {
    let descending = if rule.direction == "countdown" {
        true
    } else if rule.direction == "countup" {
        false
    } else {
        start >= end
    };
    let mut values = Vec::new();
    if descending {
        let mut current = start;
        while current >= end {
            values.push(current);
            if values.len() > rule.max_items {
                return None;
            }
            current -= 1;
        }
    } else {
        let mut current = start;
        while current <= end {
            values.push(current);
            if values.len() > rule.max_items {
                return None;
            }
            current += 1;
        }
    }
    Some(values)
}

pub fn extract_i64_numbers(text: &str) -> Vec<i64> {
    let mut numbers = Vec::new();
    let mut ascii = String::new();
    let mut chinese = String::new();
    for ch in text.chars() {
        if ch.is_ascii_digit() || (ch == '-' && ascii.is_empty() && chinese.is_empty()) {
            flush_chinese_number(&mut chinese, &mut numbers);
            ascii.push(ch);
        } else if is_chinese_number_char(ch) {
            flush_ascii_number(&mut ascii, &mut numbers);
            chinese.push(ch);
        } else {
            flush_ascii_number(&mut ascii, &mut numbers);
            flush_chinese_number(&mut chinese, &mut numbers);
        }
    }
    flush_ascii_number(&mut ascii, &mut numbers);
    flush_chinese_number(&mut chinese, &mut numbers);
    numbers
}

fn flush_ascii_number(current: &mut String, numbers: &mut Vec<i64>) {
    if !current.is_empty() {
        if let Ok(value) = current.parse::<i64>() {
            numbers.push(value);
        }
        current.clear();
    }
}

fn flush_chinese_number(current: &mut String, numbers: &mut Vec<i64>) {
    if !current.is_empty() {
        if let Some(value) = parse_chinese_i64(current) {
            numbers.push(value);
        }
        current.clear();
    }
}

fn is_chinese_number_char(ch: char) -> bool {
    matches!(
        ch,
        '零' | '〇'
            | '一'
            | '二'
            | '两'
            | '三'
            | '四'
            | '五'
            | '六'
            | '七'
            | '八'
            | '九'
            | '十'
            | '百'
            | '千'
    )
}

fn chinese_digit_value(ch: char) -> Option<i64> {
    match ch {
        '零' | '〇' => Some(0),
        '一' => Some(1),
        '二' | '两' => Some(2),
        '三' => Some(3),
        '四' => Some(4),
        '五' => Some(5),
        '六' => Some(6),
        '七' => Some(7),
        '八' => Some(8),
        '九' => Some(9),
        _ => None,
    }
}

fn parse_chinese_i64(text: &str) -> Option<i64> {
    if text.is_empty() {
        return None;
    }
    let mut total = 0i64;
    let mut current = 0i64;
    let mut saw_unit = false;
    for ch in text.chars() {
        match ch {
            '十' => {
                total += current.max(1) * 10;
                current = 0;
                saw_unit = true;
            }
            '百' => {
                total += current.max(1) * 100;
                current = 0;
                saw_unit = true;
            }
            '千' => {
                total += current.max(1) * 1000;
                current = 0;
                saw_unit = true;
            }
            other => {
                current = chinese_digit_value(other)?;
            }
        }
    }
    let value = if saw_unit { total + current } else { current };
    Some(value)
}

#[cfg(test)]
mod tests {
    use super::{builtin_reminder_rules, reminder_delay_seconds};

    #[test]
    fn builtin_reminder_rule_has_cn_hint_and_seconds_parser_works() {
        let rules = builtin_reminder_rules();
        assert_eq!(rules.len(), 1);
        assert!(rules[0].hints.iter().any(|hint| hint == "叫我"));
        assert_eq!(reminder_delay_seconds("两秒后叫我", 60), Some(2));
    }
}
