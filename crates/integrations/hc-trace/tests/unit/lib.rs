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

#[test]
fn merges_thread_context_into_event() {
    let _guard = push_trace_context(
        TraceContext::default()
            .with_run_id("run.1")
            .with_flow_id("flow.1")
            .with_component("test-component")
            .with_field("tenant_id", "local"),
    );
    let event = merge_trace_context(TraceEvent::info("", "stage", "action", "message"));
    assert_eq!(event.run_id.as_deref(), Some("run.1"));
    assert_eq!(event.flow_id.as_deref(), Some("flow.1"));
    assert_eq!(event.component, "test-component");
    assert_eq!(
        event.fields.get("tenant_id").map(String::as_str),
        Some("local")
    );
}
