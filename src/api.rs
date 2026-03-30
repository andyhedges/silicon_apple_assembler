use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::post,
    Json, Router,
};
use std::collections::HashMap;
use std::sync::Arc;
use tracing::{error, info, warn};

use crate::analyzer;
use crate::compiler;
use crate::executor::ExecutionSlot;
use crate::harness;
use crate::models::{OutputData, PipelineError, RunRequest, RunResponse};
use crate::wire_format;

/// Shared application state
pub struct AppState {
    pub rate_limiter: crate::rate_limiter::RateLimiter,
    pub execution_slot: Arc<ExecutionSlot>,
    /// The expected Bearer token. If None, any non-empty token is accepted.
    pub bearer_token: Option<String>,
}

/// Create a router with a specific required Bearer token.
/// Requests must include `Authorization: Bearer <token>` matching this value.
pub fn create_router_with_token(token: &str) -> Router {
    let state = Arc::new(AppState {
        rate_limiter: crate::rate_limiter::RateLimiter::new(),
        execution_slot: Arc::new(ExecutionSlot::new()),
        bearer_token: Some(token.to_string()),
    });

    Router::new()
        .route("/run", post(handle_run))
        .with_state(state)
}

/// Create a router that accepts any non-empty Bearer token (for testing).
pub fn create_router() -> Router {
    let state = Arc::new(AppState {
        rate_limiter: crate::rate_limiter::RateLimiter::new(),
        execution_slot: Arc::new(ExecutionSlot::new()),
        bearer_token: None,
    });

    Router::new()
        .route("/run", post(handle_run))
        .with_state(state)
}

/// Extract and validate API key from Authorization header.
/// Returns the key if valid, or None if missing/invalid/mismatched.
fn validate_api_key(headers: &HeaderMap, expected_token: &Option<String>) -> Option<String> {
    let key = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(|s| s.to_string())?;

    if key.is_empty() {
        return None;
    }

    // If an expected token is configured, the provided key must match
    if let Some(expected) = expected_token {
        if key != *expected {
            return None;
        }
    }

    Some(key)
}

async fn handle_run(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<RunRequest>,
) -> impl IntoResponse {
    let job_id = uuid::Uuid::new_v4().to_string();

    info!(job_id = %job_id, "Received /run request");

    // 1. Authentication
    let api_key = match validate_api_key(&headers, &state.bearer_token) {
        Some(key) => key,
        None => {
            warn!(job_id = %job_id, "Missing or invalid Authorization header");
            return (
                StatusCode::UNAUTHORIZED,
                Json(RunResponse::error("UNAUTHORIZED", "Missing or invalid API key")),
            );
        }
    };

    // 2. Rate limiting
    let rate_result = state.rate_limiter.check_and_record(&api_key);
    if !rate_result.allowed {
        warn!(job_id = %job_id, api_key = %api_key, "Rate limited");
        let mut response = RunResponse::rate_limited();
        response.retry_after_seconds = rate_result.retry_after_seconds;
        return (StatusCode::TOO_MANY_REQUESTS, Json(response));
    }

    // 3. Request validation
    if let Err(msg) = request.validate() {
        info!(job_id = %job_id, error = %msg, "Request validation failed");
        return (
            StatusCode::BAD_REQUEST,
            Json(RunResponse::error("VALIDATION_ERROR", &msg)),
        );
    }

    // 4. Static analysis
    info!(job_id = %job_id, "Running static analysis");
    if let Err(err) = analyzer::analyze(&request.source) {
        info!(
            job_id = %job_id,
            line = err.line,
            instruction = %err.instruction,
            "Static analysis failed"
        );
        return (
            StatusCode::BAD_REQUEST,
            Json(RunResponse::static_analysis_error(
                &err.message,
                err.line,
                &err.instruction,
            )),
        );
    }

    // 5. Generate harness
    info!(job_id = %job_id, "Generating harness");
    let harness_source =
        harness::generate_harness(&request.entrypoint, &request.inputs, request.iterations);

    // 6. Compile
    info!(job_id = %job_id, "Compiling");
    let compile_result = match compiler::compile(&job_id, &request.source, &harness_source) {
        Ok(result) => result,
        Err(e) => {
            return match e {
                PipelineError::CompileError { message } => (
                    StatusCode::BAD_REQUEST,
                    Json(RunResponse::error("COMPILE_ERROR", &message)),
                ),
                PipelineError::BinaryVerification { message } => (
                    StatusCode::BAD_REQUEST,
                    Json(RunResponse::error("BINARY_VERIFICATION_FAILED", &message)),
                ),
                PipelineError::ServerError { message } => {
                    error!(job_id = %job_id, error = %message, "Server error during compilation");
                    (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(RunResponse::error("SERVER_ERROR", &message)),
                    )
                }
                _ => {
                    error!(job_id = %job_id, "Unexpected error during compilation");
                    (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(RunResponse::error("SERVER_ERROR", "Unexpected compilation error")),
                    )
                }
            };
        }
    };

    // 7. Acquire execution slot
    let timeout = std::time::Duration::from_secs(request.timeout_seconds);
    let _guard = match state.execution_slot.try_acquire(timeout) {
        Ok(guard) => guard,
        Err(retry_after) => {
            info!(job_id = %job_id, retry_after = retry_after, "Execution slot busy");
            compiler::cleanup(&compile_result.temp_dir);
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(RunResponse::slot_busy(retry_after)),
            );
        }
    };

    // 8. Execute
    info!(job_id = %job_id, "Executing");
    let exec_result = match crate::executor::execute(
        &job_id,
        &compile_result.binary_path,
        &compile_result.temp_dir,
        request.timeout_seconds,
    )
    .await
    {
        Ok(result) => result,
        Err(e) => {
            compiler::cleanup(&compile_result.temp_dir);
            return match e {
                PipelineError::ServerError { message } => {
                    error!(job_id = %job_id, error = %message, "Server error during execution");
                    (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(RunResponse::error("SERVER_ERROR", &message)),
                    )
                }
                _ => {
                    error!(job_id = %job_id, "Unexpected error during execution");
                    (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(RunResponse::error("SERVER_ERROR", "Unexpected execution error")),
                    )
                }
            };
        }
    };

    // 9. Clean up temp directory
    compiler::cleanup(&compile_result.temp_dir);

    // 10. Handle execution result
    if exec_result.killed_by_timeout {
        info!(job_id = %job_id, "Execution timed out");
        return (
            StatusCode::OK,
            Json(RunResponse::timeout_error(&exec_result.stdout)),
        );
    }

    // Check for signal death or non-zero exit
    #[cfg(unix)]
    {
        if exec_result.exit_code.is_none() && !exec_result.killed_by_timeout {
            let msg = if exec_result.stderr.is_empty() {
                "Process terminated by signal".to_string()
            } else {
                format!("Process terminated by signal. stderr: {}", exec_result.stderr.trim())
            };
            info!(job_id = %job_id, "Process killed by signal");
            return (
                StatusCode::OK,
                Json(RunResponse::runtime_error(
                    &msg,
                    &exec_result.stdout,
                )),
            );
        }
    }

    if let Some(code) = exec_result.exit_code {
        if code != 0 {
            let msg = if exec_result.stderr.is_empty() {
                format!("Process exited with code {}", code)
            } else {
                format!("Process exited with code {}. stderr: {}", code, exec_result.stderr.trim())
            };
            info!(job_id = %job_id, exit_code = code, "Process exited with non-zero code");
            return (
                StatusCode::OK,
                Json(RunResponse::runtime_error(
                    &msg,
                    &exec_result.stdout,
                )),
            );
        }
    }

    // 11. Parse harness output
    info!(job_id = %job_id, "Parsing harness output");
    let harness_output = match wire_format::parse_harness_output(&exec_result.stdout) {
        Ok(output) => output,
        Err(msg) => {
            error!(job_id = %job_id, error = %msg, "Failed to parse harness output");
            return (
                StatusCode::OK,
                Json(RunResponse::runtime_error(
                    &format!("Failed to parse harness output: {}", msg),
                    &exec_result.stdout,
                )),
            );
        }
    };

    // 12. Build success response
    let registers = HashMap::from([("x0".to_string(), harness_output.return_value)]);
    let output = OutputData {
        stdout: harness_output.user_stdout,
        return_value: Some(harness_output.return_value),
        registers: Some(registers),
    };

    info!(
        job_id = %job_id,
        return_value = harness_output.return_value,
        mean_ns = harness_output.benchmark.mean_ns,
        "Execution complete"
    );

    (
        StatusCode::OK,
        Json(RunResponse::success(output, harness_output.benchmark)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_api_key_valid_no_expected() {
        let mut headers = HeaderMap::new();
        headers.insert("authorization", "Bearer test-key-123".parse().unwrap());
        assert_eq!(
            validate_api_key(&headers, &None),
            Some("test-key-123".to_string())
        );
    }

    #[test]
    fn test_validate_api_key_valid_matching() {
        let mut headers = HeaderMap::new();
        headers.insert("authorization", "Bearer secret-token".parse().unwrap());
        let expected = Some("secret-token".to_string());
        assert_eq!(
            validate_api_key(&headers, &expected),
            Some("secret-token".to_string())
        );
    }

    #[test]
    fn test_validate_api_key_wrong_token() {
        let mut headers = HeaderMap::new();
        headers.insert("authorization", "Bearer wrong-token".parse().unwrap());
        let expected = Some("correct-token".to_string());
        assert_eq!(validate_api_key(&headers, &expected), None);
    }

    #[test]
    fn test_validate_api_key_missing() {
        let headers = HeaderMap::new();
        assert_eq!(validate_api_key(&headers, &None), None);
    }

    #[test]
    fn test_validate_api_key_wrong_scheme() {
        let mut headers = HeaderMap::new();
        headers.insert("authorization", "Basic abc123".parse().unwrap());
        assert_eq!(validate_api_key(&headers, &None), None);
    }

    #[test]
    fn test_validate_api_key_empty_bearer() {
        let mut headers = HeaderMap::new();
        headers.insert("authorization", "Bearer ".parse().unwrap());
        assert_eq!(validate_api_key(&headers, &None), None);
    }
}