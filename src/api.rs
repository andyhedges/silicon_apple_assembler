use axum::{
    extract::{ConnectInfo, State},
    http::{HeaderMap, StatusCode},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde_json::json;
use std::collections::HashMap;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::StreamExt as _;
use tracing::{error, info, warn};

use crate::analyzer;
use crate::compiler;
use crate::events::{emit, EventTx};
use crate::executor::ExecutionSlot;
use crate::harness;
use crate::models::{DeployResponse, OutputData, PipelineError, RunRequest, RunResponse};
use crate::wire_format;

static DASHBOARD_HTML: &str = include_str!("dashboard.html");

/// Shared application state
pub struct AppState {
    pub rate_limiter: crate::rate_limiter::RateLimiter,
    pub execution_slot: Arc<ExecutionSlot>,
    /// The expected Bearer token. If None, any non-empty token is accepted.
    pub bearer_token: Option<String>,
    /// Directory in which to run `git pull` for POST /deploy.
    pub deploy_directory: String,
    /// Shell script to execute after a successful git pull.
    pub deploy_script: String,
    /// Broadcast channel for live dashboard events.
    pub event_tx: EventTx,
}

/// Create a router with a specific required Bearer token and deploy configuration.
/// Requests must include `Authorization: Bearer <token>` matching this value.
pub fn create_router_with_token(
    token: &str,
    deploy_directory: &str,
    deploy_script: &str,
    event_tx: EventTx,
) -> Router {
    let state = Arc::new(AppState {
        rate_limiter: crate::rate_limiter::RateLimiter::new(),
        execution_slot: Arc::new(ExecutionSlot::new()),
        bearer_token: Some(token.to_string()),
        deploy_directory: deploy_directory.to_string(),
        deploy_script: deploy_script.to_string(),
        event_tx,
    });

    Router::new()
        .route("/", get(handle_dashboard))
        .route("/events", get(handle_events))
        .route("/run", post(handle_run))
        .route("/deploy", post(handle_deploy))
        .with_state(state)
}

/// Create a router that accepts any non-empty Bearer token (for testing).
pub fn create_router() -> Router {
    let state = Arc::new(AppState {
        rate_limiter: crate::rate_limiter::RateLimiter::new(),
        execution_slot: Arc::new(ExecutionSlot::new()),
        bearer_token: None,
        deploy_directory: String::new(),
        deploy_script: String::new(),
        event_tx: crate::events::new_channel(),
    });

    Router::new()
        .route("/", get(handle_dashboard))
        .route("/events", get(handle_events))
        .route("/run", post(handle_run))
        .route("/deploy", post(handle_deploy))
        .with_state(state)
}

/// Serve the live activity dashboard.
async fn handle_dashboard() -> impl IntoResponse {
    Html(DASHBOARD_HTML)
}

/// Server-Sent Events stream for the live dashboard.
async fn handle_events(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    use axum::response::sse::{Event, KeepAlive, Sse};

    let rx = state.event_tx.subscribe();
    let stream = BroadcastStream::new(rx).filter_map(|result| {
        let item: Option<Result<Event, Infallible>> = result.ok().and_then(|event| {
            let data = serde_json::to_string(&event).ok()?;
            Some(Ok(Event::default().data(data)))
        });
        item
    });
    Sse::new(stream).keep_alive(KeepAlive::default())
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

/// Resolve the client IP from headers (proxy-aware) or direct connection info.
fn resolve_ip(connect_info: Option<ConnectInfo<SocketAddr>>, headers: &HeaderMap) -> Option<String> {
    if let Some(xff) = headers.get("x-forwarded-for") {
        if let Ok(s) = xff.to_str() {
            let first = s.split(',').next().unwrap_or("").trim();
            if !first.is_empty() {
                return Some(first.to_string());
            }
        }
    }
    if let Some(real_ip) = headers.get("x-real-ip") {
        if let Ok(s) = real_ip.to_str() {
            let s = s.trim();
            if !s.is_empty() {
                return Some(s.to_string());
            }
        }
    }
    connect_info.map(|c| c.0.ip().to_string())
}

async fn handle_run(
    State(state): State<Arc<AppState>>,
    connect_info: Option<ConnectInfo<SocketAddr>>,
    headers: HeaderMap,
    Json(request): Json<RunRequest>,
) -> impl IntoResponse {
    let job_id = uuid::Uuid::new_v4().to_string();
    let ip = resolve_ip(connect_info, &headers);

    info!(job_id = %job_id, "Received /run request");
    emit(
        &state.event_tx, "info", "request_received",
        Some(&job_id), ip.as_deref(),
        &format!("POST /run — {} bytes, {} iter, {}s timeout",
            request.source.len(), request.iterations, request.timeout_seconds),
        json!({
            "source_len": request.source.len(),
            "entrypoint": &request.entrypoint,
            "iterations": request.iterations,
            "timeout_seconds": request.timeout_seconds
        }),
    );

    // 1. Authentication
    let api_key = match validate_api_key(&headers, &state.bearer_token) {
        Some(key) => key,
        None => {
            warn!(job_id = %job_id, "Missing or invalid Authorization header");
            emit(&state.event_tx, "warn", "auth_failed", Some(&job_id), ip.as_deref(),
                "Missing or invalid API key", json!({}));
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
        emit(&state.event_tx, "warn", "rate_limited", Some(&job_id), ip.as_deref(),
            "Rate limit exceeded",
            json!({"retry_after_seconds": rate_result.retry_after_seconds}));
        let mut response = RunResponse::rate_limited();
        response.retry_after_seconds = rate_result.retry_after_seconds;
        return (StatusCode::TOO_MANY_REQUESTS, Json(response));
    }

    // 3. Request validation
    if let Err(msg) = request.validate() {
        info!(job_id = %job_id, error = %msg, "Request validation failed");
        emit(&state.event_tx, "warn", "validation_failed", Some(&job_id), ip.as_deref(),
            &format!("Validation error: {}", msg),
            json!({"error": msg}));
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
        emit(&state.event_tx, "warn", "analysis_failed", Some(&job_id), ip.as_deref(),
            &format!("Forbidden: {} (line {})", err.instruction, err.line),
            json!({"message": &err.message, "instruction": &err.instruction, "line": err.line}));
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
    emit(&state.event_tx, "info", "compile_started", Some(&job_id), ip.as_deref(),
        "Assembling and linking", json!({}));

    let compile_result = match compiler::compile(&job_id, &request.source, &harness_source) {
        Ok(result) => {
            emit(&state.event_tx, "info", "compile_completed", Some(&job_id), ip.as_deref(),
                "Compilation succeeded", json!({}));
            result
        }
        Err(PipelineError::CompileError { message }) => {
            emit(&state.event_tx, "error", "compile_failed", Some(&job_id), ip.as_deref(),
                &format!("Compile error: {}", message),
                json!({"error": &message}));
            return (
                StatusCode::BAD_REQUEST,
                Json(RunResponse::error("COMPILE_ERROR", &message)),
            );
        }
        Err(PipelineError::BinaryVerification { message }) => {
            emit(&state.event_tx, "error", "compile_failed", Some(&job_id), ip.as_deref(),
                &format!("Binary verification: {}", message),
                json!({"error": &message}));
            return (
                StatusCode::BAD_REQUEST,
                Json(RunResponse::error("BINARY_VERIFICATION_FAILED", &message)),
            );
        }
        Err(PipelineError::ServerError { message }) => {
            error!(job_id = %job_id, error = %message, "Server error during compilation");
            emit(&state.event_tx, "error", "compile_failed", Some(&job_id), ip.as_deref(),
                &format!("Server error: {}", message),
                json!({"error": &message}));
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(RunResponse::error("SERVER_ERROR", &message)),
            );
        }
        Err(_) => {
            error!(job_id = %job_id, "Unexpected error during compilation");
            emit(&state.event_tx, "error", "compile_failed", Some(&job_id), ip.as_deref(),
                "Unexpected compilation error", json!({}));
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(RunResponse::error("SERVER_ERROR", "Unexpected compilation error")),
            );
        }
    };

    // 7. Acquire execution slot
    let timeout = std::time::Duration::from_secs(request.timeout_seconds);
    let _guard = match state.execution_slot.try_acquire(timeout) {
        Ok(guard) => guard,
        Err(retry_after) => {
            info!(job_id = %job_id, retry_after = retry_after, "Execution slot busy");
            emit(&state.event_tx, "warn", "slot_busy", Some(&job_id), ip.as_deref(),
                &format!("Execution slot busy, retry in {}s", retry_after),
                json!({"retry_after_seconds": retry_after}));
            compiler::cleanup(&compile_result.temp_dir);
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(RunResponse::slot_busy(retry_after)),
            );
        }
    };

    // 8. Execute
    info!(job_id = %job_id, "Executing");
    emit(&state.event_tx, "info", "execute_started", Some(&job_id), ip.as_deref(),
        &format!("Executing with {}s timeout", request.timeout_seconds),
        json!({"timeout_seconds": request.timeout_seconds, "iterations": request.iterations}));

    let exec_result = match crate::executor::execute(
        &job_id,
        &compile_result.binary_path,
        &compile_result.temp_dir,
        request.timeout_seconds,
    )
    .await
    {
        Ok(result) => result,
        Err(PipelineError::ServerError { message }) => {
            error!(job_id = %job_id, error = %message, "Server error during execution");
            emit(&state.event_tx, "error", "execute_error", Some(&job_id), ip.as_deref(),
                &format!("Server error: {}", message), json!({"error": &message}));
            compiler::cleanup(&compile_result.temp_dir);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(RunResponse::error("SERVER_ERROR", &message)),
            );
        }
        Err(_) => {
            error!(job_id = %job_id, "Unexpected error during execution");
            emit(&state.event_tx, "error", "execute_error", Some(&job_id), ip.as_deref(),
                "Unexpected execution error", json!({}));
            compiler::cleanup(&compile_result.temp_dir);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(RunResponse::error("SERVER_ERROR", "Unexpected execution error")),
            );
        }
    };

    // 9. Clean up temp directory
    compiler::cleanup(&compile_result.temp_dir);

    // 10. Handle execution result
    if exec_result.killed_by_timeout {
        info!(job_id = %job_id, "Execution timed out");
        emit(&state.event_tx, "warn", "execute_timeout", Some(&job_id), ip.as_deref(),
            &format!("Timed out after {}s", request.timeout_seconds), json!({}));
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
            emit(&state.event_tx, "warn", "execute_signal", Some(&job_id), ip.as_deref(),
                "Process killed by signal",
                json!({"stderr": exec_result.stderr.trim()}));
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
            emit(&state.event_tx, "warn", "execute_failed", Some(&job_id), ip.as_deref(),
                &format!("Exit code {}", code),
                json!({"exit_code": code, "stderr": exec_result.stderr.trim()}));
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
            emit(&state.event_tx, "error", "execute_error", Some(&job_id), ip.as_deref(),
                &format!("Failed to parse harness output: {}", msg),
                json!({"error": msg, "stdout": &exec_result.stdout}));
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
        stdout: harness_output.user_stdout.clone(),
        return_value: Some(harness_output.return_value),
        registers: Some(registers),
    };

    info!(
        job_id = %job_id,
        return_value = harness_output.return_value,
        mean_ns = harness_output.benchmark.mean_ns,
        "Execution complete"
    );
    emit(&state.event_tx, "info", "execute_completed", Some(&job_id), ip.as_deref(),
        &format!("rv={} mean={}ns min={}ns max={}ns",
            harness_output.return_value,
            harness_output.benchmark.mean_ns,
            harness_output.benchmark.min_ns,
            harness_output.benchmark.max_ns),
        json!({
            "return_value": harness_output.return_value,
            "mean_ns": harness_output.benchmark.mean_ns,
            "min_ns": harness_output.benchmark.min_ns,
            "max_ns": harness_output.benchmark.max_ns,
            "iterations": harness_output.benchmark.iterations,
            "stdout": harness_output.user_stdout.trim()
        }));

    (
        StatusCode::OK,
        Json(RunResponse::success(output, harness_output.benchmark)),
    )
}

async fn handle_deploy(
    State(state): State<Arc<AppState>>,
    connect_info: Option<ConnectInfo<SocketAddr>>,
    headers: HeaderMap,
) -> Response {
    let job_id = uuid::Uuid::new_v4().to_string();
    let ip = resolve_ip(connect_info, &headers);

    info!(job_id = %job_id, "Received /deploy request");
    emit(&state.event_tx, "info", "deploy_started", Some(&job_id), ip.as_deref(),
        "POST /deploy", json!({}));

    // 1. Authentication
    if validate_api_key(&headers, &state.bearer_token).is_none() {
        warn!(job_id = %job_id, "Missing or invalid Authorization header");
        emit(&state.event_tx, "warn", "auth_failed", Some(&job_id), ip.as_deref(),
            "Missing or invalid API key", json!({}));
        return (
            StatusCode::UNAUTHORIZED,
            Json(RunResponse::error("UNAUTHORIZED", "Missing or invalid API key")),
        )
            .into_response();
    }

    // 2. Check deploy config is present
    if state.deploy_directory.is_empty() || state.deploy_script.is_empty() {
        error!(job_id = %job_id, "Deploy directory or script not configured");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(RunResponse::error("SERVER_ERROR", "Deploy not configured on this server")),
        )
            .into_response();
    }

    // 3. Run git pull
    info!(job_id = %job_id, directory = %state.deploy_directory, "Running git pull");
    emit(&state.event_tx, "info", "git_pull_started", Some(&job_id), ip.as_deref(),
        &format!("git pull in {}", state.deploy_directory), json!({}));

    let git_result = tokio::time::timeout(
        std::time::Duration::from_secs(300),
        tokio::process::Command::new("git")
            .args(["pull"])
            .current_dir(&state.deploy_directory)
            .output(),
    )
    .await;

    let git_output = match git_result {
        Err(_) => {
            warn!(job_id = %job_id, "git pull timed out");
            emit(&state.event_tx, "warn", "git_pull_timeout", Some(&job_id), ip.as_deref(),
                "git pull timed out after 300s", json!({}));
            return (StatusCode::OK, Json(DeployResponse::git_timeout())).into_response();
        }
        Ok(Err(e)) => {
            error!(job_id = %job_id, error = %e, "Failed to spawn git");
            emit(&state.event_tx, "error", "git_pull_failed", Some(&job_id), ip.as_deref(),
                &format!("Failed to spawn git: {}", e), json!({"error": e.to_string()}));
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(RunResponse::error("SERVER_ERROR", &format!("Failed to run git: {}", e))),
            )
                .into_response();
        }
        Ok(Ok(output)) => output,
    };

    let git_exit = git_output.status.code();
    let git_out = combine_output(&git_output.stdout, &git_output.stderr);
    info!(job_id = %job_id, exit_code = ?git_exit, output = %git_out, "git pull completed");

    if !git_output.status.success() {
        emit(&state.event_tx, "error", "git_pull_failed", Some(&job_id), ip.as_deref(),
            &format!("git pull failed (exit {:?})", git_exit),
            json!({"exit_code": git_exit, "output": &git_out}));
        return (StatusCode::OK, Json(DeployResponse::git_failed(git_exit, git_out))).into_response();
    }

    emit(&state.event_tx, "info", "git_pull_completed", Some(&job_id), ip.as_deref(),
        &format!("git pull ok (exit {:?})", git_exit),
        json!({"exit_code": git_exit, "output": &git_out}));

    // 4. Run deploy script
    info!(job_id = %job_id, script = %state.deploy_script, "Running deploy script");
    emit(&state.event_tx, "info", "script_started", Some(&job_id), ip.as_deref(),
        &format!("Running {}", state.deploy_script), json!({}));

    let script_result = tokio::time::timeout(
        std::time::Duration::from_secs(300),
        tokio::process::Command::new("sh")
            .arg(&state.deploy_script)
            .current_dir(&state.deploy_directory)
            .output(),
    )
    .await;

    let script_output = match script_result {
        Err(_) => {
            warn!(job_id = %job_id, "Deploy script timed out");
            emit(&state.event_tx, "warn", "script_timeout", Some(&job_id), ip.as_deref(),
                "Deploy script timed out after 300s", json!({}));
            return (StatusCode::OK, Json(DeployResponse::script_timeout(git_exit, git_out)))
                .into_response();
        }
        Ok(Err(e)) => {
            error!(job_id = %job_id, error = %e, "Failed to spawn deploy script");
            emit(&state.event_tx, "error", "script_failed", Some(&job_id), ip.as_deref(),
                &format!("Failed to spawn script: {}", e), json!({"error": e.to_string()}));
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(RunResponse::error(
                    "SERVER_ERROR",
                    &format!("Failed to run script: {}", e),
                )),
            )
                .into_response();
        }
        Ok(Ok(output)) => output,
    };

    let script_exit = script_output.status.code();
    let script_out = combine_output(&script_output.stdout, &script_output.stderr);
    info!(job_id = %job_id, exit_code = ?script_exit, output = %script_out, "Deploy script completed");

    if !script_output.status.success() {
        emit(&state.event_tx, "error", "script_failed", Some(&job_id), ip.as_deref(),
            &format!("Deploy script failed (exit {:?})", script_exit),
            json!({"exit_code": script_exit, "output": &script_out}));
        return (
            StatusCode::OK,
            Json(DeployResponse::script_failed(git_exit, git_out, script_exit, script_out)),
        )
            .into_response();
    }

    emit(&state.event_tx, "info", "script_completed", Some(&job_id), ip.as_deref(),
        &format!("Deploy script ok (exit {:?})", script_exit),
        json!({"exit_code": script_exit, "output": &script_out}));

    (
        StatusCode::OK,
        Json(DeployResponse::success(git_exit, git_out, script_exit, script_out)),
    )
        .into_response()
}

/// Combine stdout and stderr bytes into a single UTF-8 string.
fn combine_output(stdout: &[u8], stderr: &[u8]) -> String {
    let out = String::from_utf8_lossy(stdout);
    let err = String::from_utf8_lossy(stderr);
    match (out.is_empty(), err.is_empty()) {
        (true, true) => String::new(),
        (false, true) => out.into_owned(),
        (true, false) => err.into_owned(),
        (false, false) => format!("{}{}", out, err),
    }
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
