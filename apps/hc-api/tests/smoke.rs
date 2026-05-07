use std::path::PathBuf;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use hc_api::{AppState, build_router};
use hc_service::ServiceConfig;
use tower::ServiceExt;

fn test_state() -> AppState {
    let workspace_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    AppState {
        service: ServiceConfig::new(workspace_root),
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
}
