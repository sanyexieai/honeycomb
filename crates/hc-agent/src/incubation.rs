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
    record
        .tags
        .push(format!("promotion:{:?}", report.promotion).to_ascii_lowercase());
    record
}

#[cfg(test)]
#[path = "../tests/unit/incubation.rs"]
mod tests;
