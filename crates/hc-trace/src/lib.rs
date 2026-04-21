use hc_responder::ResponderKind;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActivityItemView {
    pub kind: String,
    pub stage: String,
    pub actor: String,
    pub title: String,
    pub detail: String,
}

impl ActivityItemView {
    pub fn new(
        kind: impl Into<String>,
        stage: impl Into<String>,
        actor: impl Into<String>,
        title: impl Into<String>,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            kind: kind.into(),
            stage: stage.into(),
            actor: actor.into(),
            title: title.into(),
            detail: detail.into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DecisionTraceView {
    pub code: String,
    pub stage: String,
    pub subject: String,
    pub outcome: String,
    pub detail: String,
}

impl DecisionTraceView {
    pub fn new(
        code: impl Into<String>,
        stage: impl Into<String>,
        subject: impl Into<String>,
        outcome: impl Into<String>,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            code: code.into(),
            stage: stage.into(),
            subject: subject.into(),
            outcome: outcome.into(),
            detail: detail.into(),
        }
    }
}

pub fn agent_code_from(role: &str, instance_id: &str) -> String {
    format!(
        "AGT-{}-{}",
        role.to_ascii_uppercase(),
        compact_code(instance_id)
    )
}

pub fn behavior_mode_code_from(responder_kind: Option<ResponderKind>) -> String {
    match responder_kind {
        Some(ResponderKind::Llm) => "MODE-LLM-AUTO".to_owned(),
        Some(ResponderKind::Human) => "MODE-HUMAN-MANUAL".to_owned(),
        Some(ResponderKind::Rule) => "MODE-RULE-AUTO".to_owned(),
        Some(ResponderKind::Script) => "MODE-SCRIPT-AUTO".to_owned(),
        None => "MODE-UNBOUND".to_owned(),
    }
}

pub fn code_from(prefix: &str, value: &str) -> String {
    format!("{prefix}-{}", compact_code(value))
}

pub fn summarize_trace_body(body: &str) -> String {
    const LIMIT: usize = 80;
    if body.chars().count() <= LIMIT {
        return body.to_owned();
    }

    let shortened = body.chars().take(LIMIT).collect::<String>();
    format!("{shortened}...")
}

pub fn compact_code(value: &str) -> String {
    value
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .take(8)
        .collect::<String>()
        .to_ascii_uppercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generates_codes_and_modes() {
        assert_eq!(
            agent_code_from("planner", "instance.0001"),
            "AGT-PLANNER-INSTANCE"
        );
        assert_eq!(
            behavior_mode_code_from(Some(ResponderKind::Human)),
            "MODE-HUMAN-MANUAL"
        );
        assert_eq!(code_from("TASK", "task.ui.123"), "TASK-TASKUI12");
    }
}
