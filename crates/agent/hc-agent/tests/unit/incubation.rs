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
