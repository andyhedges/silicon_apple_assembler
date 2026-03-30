use axum::body::Body;
use axum::http::{Request, StatusCode};
use std::sync::Arc;
use std::time::Duration;
use tower::ServiceExt;

// ============================================================
// Tests exercising the real API router (no macOS toolchain needed)
// ============================================================

#[tokio::test]
async fn test_missing_auth_returns_401() {
    let app = arm64_sandbox::api::create_router();

    let req = Request::builder()
        .method("POST")
        .uri("/run")
        .header("content-type", "application/json")
        .body(Body::from(r#"{"source": "ret"}"#))
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn test_invalid_iterations_returns_400() {
    let app = arm64_sandbox::api::create_router();

    let req = Request::builder()
        .method("POST")
        .uri("/run")
        .header("content-type", "application/json")
        .header("authorization", "Bearer test-key")
        .body(Body::from(r#"{"source": "ret", "iterations": 0}"#))
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn test_invalid_timeout_returns_400() {
    let app = arm64_sandbox::api::create_router();

    let req = Request::builder()
        .method("POST")
        .uri("/run")
        .header("content-type", "application/json")
        .header("authorization", "Bearer test-key")
        .body(Body::from(
            r#"{"source": "ret", "timeout_seconds": 999}"#,
        ))
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn test_oversized_source_returns_400() {
    let app = arm64_sandbox::api::create_router();
    let big_source = "x".repeat(64 * 1024 + 1);
    let body = serde_json::json!({ "source": big_source });

    let req = Request::builder()
        .method("POST")
        .uri("/run")
        .header("content-type", "application/json")
        .header("authorization", "Bearer test-key")
        .body(Body::from(serde_json::to_string(&body).unwrap()))
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn test_invalid_entrypoint_returns_400() {
    let app = arm64_sandbox::api::create_router();
    let body = serde_json::json!({
        "source": "ret",
        "entrypoint": "no_underscore"
    });

    let req = Request::builder()
        .method("POST")
        .uri("/run")
        .header("content-type", "application/json")
        .header("authorization", "Bearer test-key")
        .body(Body::from(serde_json::to_string(&body).unwrap()))
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn test_invalid_register_returns_400() {
    let app = arm64_sandbox::api::create_router();
    let body = serde_json::json!({
        "source": "ret",
        "inputs": { "x9": 1 }
    });

    let req = Request::builder()
        .method("POST")
        .uri("/run")
        .header("content-type", "application/json")
        .header("authorization", "Bearer test-key")
        .body(Body::from(serde_json::to_string(&body).unwrap()))
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn test_static_analysis_rejects_svc() {
    let app = arm64_sandbox::api::create_router();

    let req = Request::builder()
        .method("POST")
        .uri("/run")
        .header("content-type", "application/json")
        .header("authorization", "Bearer test-key")
        .body(Body::from(r#"{"source": "svc #0x80"}"#))
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["error_code"], "STATIC_ANALYSIS_FAILED");
    assert_eq!(json["detail"]["instruction"], "svc");
    assert!(json["detail"]["line"].as_u64().is_some());
}

#[tokio::test]
async fn test_static_analysis_rejects_indirect_branch() {
    let app = arm64_sandbox::api::create_router();

    let req = Request::builder()
        .method("POST")
        .uri("/run")
        .header("content-type", "application/json")
        .header("authorization", "Bearer test-key")
        .body(Body::from(r#"{"source": "br x8"}"#))
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["error_code"], "STATIC_ANALYSIS_FAILED");
    assert_eq!(json["detail"]["instruction"], "br");
}

#[tokio::test]
async fn test_static_analysis_rejects_macro() {
    let app = arm64_sandbox::api::create_router();

    let req = Request::builder()
        .method("POST")
        .uri("/run")
        .header("content-type", "application/json")
        .header("authorization", "Bearer test-key")
        .body(Body::from(
            r#"{"source": ".macro mymacro\nnop\n.endmacro"}"#,
        ))
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["error_code"], "STATIC_ANALYSIS_FAILED");
}

// ============================================================
// Rate limiting test using shared real state
// ============================================================

#[tokio::test]
async fn test_rate_limiting_enforces_per_key_limit() {
    let rate_limiter = arm64_sandbox::rate_limiter::RateLimiter::new();

    // Exhaust the per-minute limit (60 requests)
    for i in 0..60 {
        let result = rate_limiter.check_and_record("rate-test-key");
        assert!(
            result.allowed,
            "Request {} should be allowed under the limit",
            i
        );
    }

    // 61st request should be rejected
    let result = rate_limiter.check_and_record("rate-test-key");
    assert!(!result.allowed, "Request 61 should be rate limited");
    assert!(result.retry_after_seconds.is_some());

    // A different key should still be allowed
    let result = rate_limiter.check_and_record("other-key");
    assert!(result.allowed, "Different key should not be rate limited");
}

// ============================================================
// Execution slot tests using real ExecutionSlot component
// ============================================================

#[tokio::test]
async fn test_execution_slot_returns_busy_when_occupied() {
    let slot = Arc::new(arm64_sandbox::executor::ExecutionSlot::new());

    let _guard = slot.try_acquire(Duration::from_secs(10)).unwrap();

    // Second acquisition should fail with retry_after
    let result = slot.try_acquire(Duration::from_secs(10));
    assert!(result.is_err());
    let retry_after = result.unwrap_err();
    assert!(retry_after >= 1);
    assert!(retry_after <= 10);
}

#[tokio::test]
async fn test_execution_slot_releases_deterministically_on_drop() {
    let slot = Arc::new(arm64_sandbox::executor::ExecutionSlot::new());

    {
        let _guard = slot.try_acquire(Duration::from_secs(5)).unwrap();
        // guard dropped here
    }

    // Should be immediately available after guard is dropped
    let result = slot.try_acquire(Duration::from_secs(5));
    assert!(result.is_ok(), "Slot should be free after guard is dropped");
}

#[tokio::test]
async fn test_execution_slot_retry_after_decreases_over_time() {
    let slot = Arc::new(arm64_sandbox::executor::ExecutionSlot::new());

    let _guard = slot.try_acquire(Duration::from_secs(5)).unwrap();

    let retry1 = slot.try_acquire(Duration::from_secs(5)).unwrap_err();

    // Wait a bit
    tokio::time::sleep(Duration::from_secs(1)).await;

    let retry2 = slot.try_acquire(Duration::from_secs(5)).unwrap_err();

    // retry_after should have decreased (or stayed same if rounding)
    assert!(retry2 <= retry1);
}

// ============================================================
// Tests requiring macOS toolchain (gated with #[ignore])
// ============================================================

/// End-to-end test matching the §12 example: sum 1..100 = 5050
#[tokio::test]
#[ignore]
async fn test_end_to_end_sum_example() {
    let app = arm64_sandbox::api::create_router();

    let source = ".global _user_entry\n\
                  .align 2\n\
                  \n\
                  _user_entry:\n\
                      stp x29, x30, [sp, #-16]!\n\
                      mov x29, sp\n\
                      mov x2, #0\n\
                      mov x3, x0\n\
                  _loop:\n\
                      cbz x3, _done\n\
                      add x2, x2, x3\n\
                      sub x3, x3, #1\n\
                      b _loop\n\
                  _done:\n\
                      mov x0, x2\n\
                      ldp x29, x30, [sp], #16\n\
                      ret\n";

    let body = serde_json::json!({
        "source": source,
        "inputs": { "x0": 100 },
        "iterations": 10000,
        "timeout_seconds": 5
    });

    let req = Request::builder()
        .method("POST")
        .uri("/run")
        .header("content-type", "application/json")
        .header("authorization", "Bearer test-key")
        .body(Body::from(serde_json::to_string(&body).unwrap()))
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();

    assert_eq!(
        json["status"], "ok",
        "Expected status 'ok' but got full response: {}",
        serde_json::to_string_pretty(&json).unwrap()
    );
    assert_eq!(json["output"]["return_value"], 5050);
    assert_eq!(json["output"]["registers"]["x0"], 5050);
    assert!(json["benchmark"]["iterations"].as_u64().unwrap() > 0);
    assert!(json["benchmark"]["mean_ns"].as_u64().unwrap() > 0);
}

/// Test that a timeout is enforced for an infinite loop
#[tokio::test]
#[ignore]
async fn test_timeout_enforcement() {
    let app = arm64_sandbox::api::create_router();

    let body = serde_json::json!({
        "source": ".global _user_entry\n.align 2\n_user_entry:\n    b _user_entry\n",
        "iterations": 1,
        "timeout_seconds": 2
    });

    let req = Request::builder()
        .method("POST")
        .uri("/run")
        .header("content-type", "application/json")
        .header("authorization", "Bearer test-key")
        .body(Body::from(serde_json::to_string(&body).unwrap()))
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();

    assert_eq!(json["status"], "error");
    let error_code = json["error_code"].as_str().unwrap();
    assert!(
        error_code == "TIMEOUT" || error_code == "RUNTIME_ERROR",
        "Expected TIMEOUT or RUNTIME_ERROR, got {}",
        error_code
    );
}