use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use axum::body::{Body, to_bytes};
use axum::http::{Method, Request, StatusCode, header};
use hc_api::{AppState, build_router};
use hc_service::ServiceConfig;
use tower::ServiceExt;

fn test_state() -> AppState {
    let workspace_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    AppState {
        service: ServiceConfig::new(workspace_root),
        followup_headless_delivered_messages_total: Arc::new(Mutex::new(HashMap::new())),
        api_scheduler_dispatch_totals: Arc::new(Mutex::new(HashMap::new())),
    }
}

#[tokio::test]
async fn health_returns_ok() {
    let app = build_router(test_state(), "https://example.com".to_string());
    let response = app
        .oneshot(
            Request::builder()
                .uri("/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn schedule_recovery_get_endpoints_return_json() {
    let app = build_router(test_state(), "https://example.com".to_string());
    let uri_fired =
        "/v1/schedules/followup-fired-events?tenant_id=local&user_id=integration-test-schedule-ro";
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(uri_fired)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read followup-fired-events body");
    let fires: serde_json::Value =
        serde_json::from_slice(&bytes).expect("followup-fired-events json");
    assert!(
        fires.is_array(),
        "expected JSON array from followup-fired-events: {fires}"
    );

    let uri_stats = "/v1/schedules/operational-stats?tenant_id=local&user_id=integration-test-schedule-ro&now_unix=42";
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(uri_stats)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read stats body");
    let stats: serde_json::Value = serde_json::from_slice(&bytes).expect("stats json");
    for key in [
        "now_unix",
        "followup_total",
        "followup_pending",
        "followup_pending_due",
        "followup_fired",
        "followup_cancelled",
        "followup_failed",
        "schedule_total",
        "schedule_active",
        "schedule_paused",
        "schedule_cancelled",
        "schedule_timed_mirror_active",
        "run_queued",
        "run_running",
        "run_succeeded",
        "run_failed",
        "run_cancelled",
    ] {
        assert!(
            stats.get(key).is_some(),
            "operational-stats missing {key}: {stats}"
        );
    }
    assert_eq!(
        stats.get("now_unix").and_then(|v| v.as_u64()),
        Some(42),
        "now_unix query should be honored: {stats}"
    );

    let dispatch_body = serde_json::json!({
        "namespace": {
            "tenant_id": "local",
            "user_id": "integration-test-schedule-ro"
        }
    })
    .to_string();
    let dispatch_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/v1/schedules/dispatch-due")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(dispatch_body.clone()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        dispatch_response.status(),
        StatusCode::OK,
        "POST dispatch-due should succeed for prometheus histogram merge"
    );

    let dq_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/v1/schedules/dispatch-queued")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(dispatch_body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        dq_response.status(),
        StatusCode::OK,
        "POST dispatch-queued should succeed for prometheus histogram merge"
    );

    let uri_prom = "/v1/schedules/metrics/prometheus?tenant_id=local&user_id=integration-test-schedule-ro&now_unix=42";
    let response = app
        .oneshot(
            Request::builder()
                .uri(uri_prom)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let ctype = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        ctype.starts_with("text/plain"),
        "unexpected content-type: {ctype}"
    );
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read prometheus body");
    let text = String::from_utf8(bytes.to_vec()).expect("prometheus utf-8");
    assert!(
        text.contains("honeycomb_scheduler_followups_total"),
        "{text}"
    );
    assert!(
        text.contains("honeycomb_scheduler_api_followup_messages_delivered_total"),
        "expected preferred follow-up delivery gauge: {text}"
    );
    assert!(
        text.contains("honeycomb_scheduler_api_followup_headless_messages_delivered_total"),
        "expected legacy follow-up delivery gauge alias: {text}"
    );
    assert!(
        text.contains("honeycomb_scheduler_api_dispatch_due_completed_total"),
        "expected hc-api prometheus dispatch counter: {text}"
    );
    assert!(
        text.contains("honeycomb_scheduler_api_scheduler_loop_tick_completed_total"),
        "expected hc-api prometheus tick counter: {text}"
    );
    assert!(
        text.contains("honeycomb_scheduler_api_dispatch_due_last_worker_wall_ms"),
        "{text}"
    );
    assert!(
        text.contains("honeycomb_scheduler_api_dispatch_due_worker_wall_ms_count"),
        "expected dispatch-due wall-time histogram in prometheus text: {text}"
    );
    assert!(
        text.contains("honeycomb_scheduler_api_dispatch_due_worker_wall_ms_bucket"),
        "{text}"
    );
    assert!(
        text.contains("honeycomb_scheduler_api_dispatch_queued_worker_wall_ms_count"),
        "expected dispatch-queued wall-time histogram in prometheus text: {text}"
    );
    assert!(
        text.contains("honeycomb_scheduler_api_dispatch_queued_worker_wall_ms_bucket"),
        "{text}"
    );
    assert!(text.contains("# EOF\n"), "{text}");
    assert!(
        text.contains(r#"tenant_id="local",user_id="integration-test-schedule-ro""#),
        "{text}"
    );
}

#[tokio::test]
async fn openapi_returns_ok() {
    let app = build_router(test_state(), "https://example.com".to_string());
    let response = app
        .oneshot(
            Request::builder()
                .uri("/openapi.json")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read openapi body");
    let spec: serde_json::Value = serde_json::from_slice(&bytes).expect("openapi json");
    assert!(
        spec["paths"]["/v1/schedules/followup-fired-events"]["get"].is_object(),
        "openapi should expose GET /v1/schedules/followup-fired-events"
    );
    assert!(
        spec["paths"]["/v1/schedules/operational-stats"]["get"].is_object(),
        "openapi should expose GET /v1/schedules/operational-stats"
    );
    assert!(
        spec["paths"]["/v1/schedules/metrics/prometheus"]["get"].is_object(),
        "openapi should expose GET /v1/schedules/metrics/prometheus"
    );
}

#[tokio::test]
async fn openapi_documents_stream_room_capabilities_schema() {
    let app = build_router(test_state(), "https://example.com".to_string());
    let response = app
        .oneshot(
            Request::builder()
                .uri("/openapi.json")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read openapi body");
    let spec: serde_json::Value = serde_json::from_slice(&bytes).expect("openapi json");
    assert!(
        spec["components"]["schemas"]["RoomCapabilitiesStreamData"].is_object(),
        "expected RoomCapabilitiesStreamData schema"
    );
    let desc = spec["paths"]["/v1/chat/stream"]["post"]["description"]
        .as_str()
        .expect("stream description");
    assert!(
        desc.contains("chat.room_capabilities"),
        "stream description should mention chat.room_capabilities: {desc}"
    );
    assert!(
        spec["paths"]["/v1/messages"]["post"]["operationId"].as_str() == Some("postMessages"),
        "expected /v1/messages POST operationId postMessages"
    );
    assert!(
        spec["paths"]["/v1/messages/stream"]["post"]["operationId"].as_str()
            == Some("streamMessages"),
        "expected /v1/messages/stream POST operationId streamMessages"
    );
    let required = spec["components"]["schemas"]["UserMessageBody"]["required"]
        .as_array()
        .expect("UserMessageBody.required");
    assert!(
        required.iter().any(|v| v.as_str() == Some("text")),
        "UserMessageBody should require text: {required:?}"
    );
    assert!(
        spec["components"]["schemas"]["UserMessageBody"]["properties"]["messages"].is_object(),
        "UserMessageBody should expose optional messages"
    );
    assert!(
        spec["components"]["schemas"]["UserMessageBody"]["properties"]["memory"]["$ref"].as_str()
            == Some("#/components/schemas/ApiMemoryQuery"),
        "UserMessageBody.memory should reference ApiMemoryQuery",
    );
    assert!(
        spec["components"]["schemas"]["UserMessageBody"]["properties"]["thinking_depth"]
            .is_object(),
        "UserMessageBody should expose optional thinking_depth"
    );
    assert!(
        spec["components"]["schemas"]["UserMessageBody"]["properties"]["active_agent_id"]
            .is_object(),
        "UserMessageBody should expose optional active_agent_id"
    );
    assert!(
        spec["components"]["schemas"]["UserMessageBody"]["properties"]["provider"].is_object(),
        "UserMessageBody should expose optional provider"
    );
    assert!(
        spec["components"]["schemas"]["UserMessageBody"]["properties"]["temperature"].is_object(),
        "UserMessageBody should expose optional temperature"
    );
    let alias = spec["components"]["schemas"]["UserMessageStreamBody"]
        .as_object()
        .expect("UserMessageStreamBody deprecated alias");
    assert_eq!(
        alias.get("deprecated").and_then(|v| v.as_bool()),
        Some(true),
        "legacy schema name should be marked deprecated",
    );
    let all_of = alias["allOf"]
        .as_array()
        .expect("UserMessageStreamBody alias allOf");
    assert_eq!(
        all_of
            .first()
            .and_then(|entry| entry.get("$ref"))
            .and_then(|value| value.as_str()),
        Some("#/components/schemas/UserMessageBody"),
    );
}
