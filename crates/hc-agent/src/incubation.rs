use hc_memory::{MemoryKind, MemoryRecord};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct IncubationObservation {
    pub kind: String,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct IncubationReport {
    pub task_id: String,
    pub instance_id: String,
    pub observations: Vec<IncubationObservation>,
    pub promotion: PromotionDecision,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PromotionDecision {
    KeepEphemeral,
    ContinueIncubating,
    PromoteToStablePersona,
}

pub fn build_memory_record_from_report(report: &IncubationReport) -> MemoryRecord {
    let summary = if report.observations.is_empty() {
        "No observations captured.".to_owned()
    } else {
        report
            .observations
            .iter()
            .map(|observation| format!("{}: {}", observation.kind, observation.detail))
            .collect::<Vec<_>>()
            .join(" | ")
    };

    let mut record = MemoryRecord::task_summary(
        report.task_id.clone(),
        report.instance_id.clone(),
        format!("Incubation Summary for {}", report.instance_id),
        summary,
    );
    record.kind = MemoryKind::Summary;
    record.tags.push("incubation".to_owned());
    record.tags.push(format!("promotion:{:?}", report.promotion).to_ascii_lowercase());
    record
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn incubation_report_can_be_converted_to_memory_record() {
        let report = IncubationReport {
            task_id: "task.demo".to_owned(),
            instance_id: "instance.0001".to_owned(),
            observations: vec![IncubationObservation {
                kind: "strength".to_owned(),
                detail: "handled review well".to_owned(),
            }],
            promotion: PromotionDecision::ContinueIncubating,
        };

        let memory = build_memory_record_from_report(&report);

        assert_eq!(memory.scope, hc_memory::MemoryScope::Task);
        assert_eq!(memory.kind, MemoryKind::Summary);
        assert!(memory.summary.contains("strength: handled review well"));
        assert!(memory.tags.iter().any(|tag| tag == "incubation"));
    }
}
