use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Request body for POST /run
#[derive(Debug, Deserialize)]
pub struct RunRequest {
    pub source: String,
    #[serde(default = "default_entrypoint")]
    pub entrypoint: String,
    #[serde(default)]
    pub inputs: HashMap<String, i64>,
    #[serde(default = "default_iterations")]
    pub iterations: u64,
    #[serde(default = "default_timeout")]
    pub timeout_seconds: u64,
}

fn default_entrypoint() -> String {
    "_user_entry".to_string()
}

fn default_iterations() -> u64 {
    1
}

fn default_timeout() -> u64 {
    10
}

/// Top-level API response
#[derive(Debug, Serialize)]
pub struct RunResponse {
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<OutputData>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub benchmark: Option<BenchmarkStats>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<ErrorDetail>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry_after_seconds: Option<u64>,
}

#[derive(Debug, Serialize)]
pub struct OutputData {
    pub stdout: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub return_value: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub registers: Option<HashMap<String, i64>>,
}

#[derive(Debug, Serialize)]
pub struct BenchmarkStats {
    pub iterations: u64,
    pub total_ns: u64,
    pub mean_ns: u64,
    pub median_ns: u64,
    pub min_ns: u64,
    pub max_ns: u64,
    pub stddev_ns: u64,
}

#[derive(Debug, Serialize)]
pub struct ErrorDetail {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instruction: Option<String>,
}

/// Internal error type for the pipeline
#[derive(Debug)]
pub enum PipelineError {
    StaticAnalysis {
        message: String,
        line: usize,
        instruction: String,
    },
    CompileError {
        message: String,
    },
    BinaryVerification {
        message: String,
    },
    RuntimeError {
        message: String,
        stdout: String,
    },
    Timeout {
        stdout: String,
    },
    ServerError {
        message: String,
    },
}

impl RunResponse {
    pub fn success(output: OutputData, benchmark: BenchmarkStats) -> Self {
        Self {
            status: "ok".to_string(),
            output: Some(output),
            benchmark: Some(benchmark),
            error_code: None,
            message: None,
            detail: None,
            retry_after_seconds: None,
        }
    }

    pub fn error(error_code: &str, message: &str) -> Self {
        Self {
            status: "error".to_string(),
            output: None,
            benchmark: None,
            error_code: Some(error_code.to_string()),
            message: Some(message.to_string()),
            detail: None,
            retry_after_seconds: None,
        }
    }

    pub fn static_analysis_error(message: &str, line: usize, instruction: &str) -> Self {
        Self {
            status: "error".to_string(),
            output: None,
            benchmark: None,
            error_code: Some("STATIC_ANALYSIS_FAILED".to_string()),
            message: Some(message.to_string()),
            detail: Some(ErrorDetail {
                line: Some(line),
                instruction: Some(instruction.to_string()),
            }),
            retry_after_seconds: None,
        }
    }

    pub fn runtime_error(message: &str, stdout: &str) -> Self {
        Self {
            status: "error".to_string(),
            output: Some(OutputData {
                stdout: stdout.to_string(),
                return_value: None,
                registers: None,
            }),
            benchmark: None,
            error_code: Some("RUNTIME_ERROR".to_string()),
            message: Some(message.to_string()),
            detail: None,
            retry_after_seconds: None,
        }
    }

    pub fn timeout_error(stdout: &str) -> Self {
        Self {
            status: "error".to_string(),
            output: Some(OutputData {
                stdout: stdout.to_string(),
                return_value: None,
                registers: None,
            }),
            benchmark: None,
            error_code: Some("TIMEOUT".to_string()),
            message: Some("Process exceeded wall-clock timeout".to_string()),
            detail: None,
            retry_after_seconds: None,
        }
    }

    pub fn slot_busy(retry_after: u64) -> Self {
        Self {
            status: "error".to_string(),
            output: None,
            benchmark: None,
            error_code: Some("EXECUTION_SLOT_BUSY".to_string()),
            message: Some(format!(
                "A benchmark is currently running. Retry in {} seconds.",
                retry_after
            )),
            detail: None,
            retry_after_seconds: Some(retry_after),
        }
    }

    pub fn rate_limited() -> Self {
        Self {
            status: "error".to_string(),
            output: None,
            benchmark: None,
            error_code: Some("RATE_LIMITED".to_string()),
            message: Some("Too many requests".to_string()),
            detail: None,
            retry_after_seconds: None,
        }
    }
}

/// Output from a single stage of the deploy pipeline
#[derive(Debug, Serialize)]
pub struct DeployStageOutput {
    pub exit_code: Option<i32>,
    pub output: String,
}

/// Response body for POST /deploy
#[derive(Debug, Serialize)]
pub struct DeployResponse {
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    pub git_pull: DeployStageOutput,
    pub script: DeployStageOutput,
}

impl DeployResponse {
    fn skipped() -> DeployStageOutput {
        DeployStageOutput { exit_code: None, output: String::new() }
    }

    pub fn success(
        git_exit: Option<i32>, git_out: String,
        script_exit: Option<i32>, script_out: String,
    ) -> Self {
        Self {
            status: "ok".to_string(),
            error_code: None,
            message: None,
            git_pull: DeployStageOutput { exit_code: git_exit, output: git_out },
            script: DeployStageOutput { exit_code: script_exit, output: script_out },
        }
    }

    pub fn git_failed(exit_code: Option<i32>, output: String) -> Self {
        Self {
            status: "error".to_string(),
            error_code: Some("GIT_PULL_FAILED".to_string()),
            message: Some("git pull exited with non-zero status".to_string()),
            git_pull: DeployStageOutput { exit_code, output },
            script: Self::skipped(),
        }
    }

    pub fn script_failed(
        git_exit: Option<i32>, git_out: String,
        script_exit: Option<i32>, script_out: String,
    ) -> Self {
        Self {
            status: "error".to_string(),
            error_code: Some("SCRIPT_FAILED".to_string()),
            message: Some("Deploy script exited with non-zero status".to_string()),
            git_pull: DeployStageOutput { exit_code: git_exit, output: git_out },
            script: DeployStageOutput { exit_code: script_exit, output: script_out },
        }
    }

    pub fn git_timeout() -> Self {
        Self {
            status: "error".to_string(),
            error_code: Some("TIMEOUT".to_string()),
            message: Some("git pull exceeded the 300s timeout".to_string()),
            git_pull: DeployStageOutput { exit_code: None, output: String::new() },
            script: Self::skipped(),
        }
    }

    pub fn script_timeout(git_exit: Option<i32>, git_out: String) -> Self {
        Self {
            status: "error".to_string(),
            error_code: Some("TIMEOUT".to_string()),
            message: Some("Deploy script exceeded the 600s timeout".to_string()),
            git_pull: DeployStageOutput { exit_code: git_exit, output: git_out },
            script: DeployStageOutput { exit_code: None, output: String::new() },
        }
    }
}

/// Validation for RunRequest
impl RunRequest {
    pub fn validate(&self) -> Result<(), String> {
        // Source must not be empty
        if self.source.is_empty() {
            return Err("source is required".to_string());
        }
        // Source max 512 KB
        if self.source.len() > 512 * 1024 {
            return Err("source exceeds 512 KB limit".to_string());
        }
        // Entrypoint must match /^_[a-zA-Z_]\w*$/
        let re = regex::Regex::new(r"^_[a-zA-Z_]\w*$").unwrap();
        if !re.is_match(&self.entrypoint) {
            return Err(format!(
                "entrypoint '{}' does not match required pattern /^_[a-zA-Z_]\\w*$/",
                self.entrypoint
            ));
        }
        // Iterations: 1–1,000,000
        if self.iterations < 1 || self.iterations > 1_000_000 {
            return Err(format!(
                "iterations must be between 1 and 1,000,000, got {}",
                self.iterations
            ));
        }
        // Timeout: 1–600
        if self.timeout_seconds < 1 || self.timeout_seconds > 600 {
            return Err(format!(
                "timeout_seconds must be between 1 and 600, got {}",
                self.timeout_seconds
            ));
        }
        // Inputs: only x0–x7
        for key in self.inputs.keys() {
            match key.as_str() {
                "x0" | "x1" | "x2" | "x3" | "x4" | "x5" | "x6" | "x7" => {}
                _ => {
                    return Err(format!(
                        "invalid input register '{}': only x0–x7 are supported",
                        key
                    ));
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_valid_request() {
        let req = RunRequest {
            source: ".global _user_entry\n_user_entry:\n    ret\n".to_string(),
            entrypoint: "_user_entry".to_string(),
            inputs: HashMap::from([("x0".to_string(), 42)]),
            iterations: 1000,
            timeout_seconds: 10,
        };
        assert!(req.validate().is_ok());
    }

    #[test]
    fn test_validate_empty_source() {
        let req = RunRequest {
            source: "".to_string(),
            entrypoint: "_user_entry".to_string(),
            inputs: HashMap::new(),
            iterations: 1,
            timeout_seconds: 10,
        };
        assert_eq!(req.validate().unwrap_err(), "source is required");
    }

    #[test]
    fn test_validate_source_too_large() {
        let req = RunRequest {
            source: "x".repeat(512 * 1024 + 1),
            entrypoint: "_user_entry".to_string(),
            inputs: HashMap::new(),
            iterations: 1,
            timeout_seconds: 10,
        };
        assert!(req.validate().unwrap_err().contains("512 KB"));
    }

    #[test]
    fn test_validate_bad_entrypoint() {
        let req = RunRequest {
            source: "ret".to_string(),
            entrypoint: "no_underscore".to_string(),
            inputs: HashMap::new(),
            iterations: 1,
            timeout_seconds: 10,
        };
        assert!(req.validate().unwrap_err().contains("entrypoint"));
    }

    #[test]
    fn test_validate_entrypoint_starting_with_digit() {
        let req = RunRequest {
            source: "ret".to_string(),
            entrypoint: "_1bad".to_string(),
            inputs: HashMap::new(),
            iterations: 1,
            timeout_seconds: 10,
        };
        // _1bad — first char after _ is a digit, which doesn't match [a-zA-Z_]
        assert!(req.validate().unwrap_err().contains("entrypoint"));
    }

    #[test]
    fn test_validate_iterations_zero() {
        let req = RunRequest {
            source: "ret".to_string(),
            entrypoint: "_user_entry".to_string(),
            inputs: HashMap::new(),
            iterations: 0,
            timeout_seconds: 10,
        };
        assert!(req.validate().unwrap_err().contains("iterations"));
    }

    #[test]
    fn test_validate_iterations_too_high() {
        let req = RunRequest {
            source: "ret".to_string(),
            entrypoint: "_user_entry".to_string(),
            inputs: HashMap::new(),
            iterations: 1_000_001,
            timeout_seconds: 10,
        };
        assert!(req.validate().unwrap_err().contains("iterations"));
    }

    #[test]
    fn test_validate_timeout_zero() {
        let req = RunRequest {
            source: "ret".to_string(),
            entrypoint: "_user_entry".to_string(),
            inputs: HashMap::new(),
            iterations: 1,
            timeout_seconds: 0,
        };
        assert!(req.validate().unwrap_err().contains("timeout_seconds"));
    }

    #[test]
    fn test_validate_timeout_too_high() {
        let req = RunRequest {
            source: "ret".to_string(),
            entrypoint: "_user_entry".to_string(),
            inputs: HashMap::new(),
            iterations: 1,
            timeout_seconds: 601,
        };
        assert!(req.validate().unwrap_err().contains("timeout_seconds"));
    }

    #[test]
    fn test_validate_invalid_register() {
        let req = RunRequest {
            source: "ret".to_string(),
            entrypoint: "_user_entry".to_string(),
            inputs: HashMap::from([("x9".to_string(), 1)]),
            iterations: 1,
            timeout_seconds: 10,
        };
        assert!(req.validate().unwrap_err().contains("x9"));
    }

    #[test]
    fn test_validate_all_valid_registers() {
        let req = RunRequest {
            source: "ret".to_string(),
            entrypoint: "_user_entry".to_string(),
            inputs: HashMap::from([
                ("x0".to_string(), 1),
                ("x1".to_string(), 2),
                ("x2".to_string(), 3),
                ("x3".to_string(), 4),
                ("x4".to_string(), 5),
                ("x5".to_string(), 6),
                ("x6".to_string(), 7),
                ("x7".to_string(), 8),
            ]),
            iterations: 1,
            timeout_seconds: 10,
        };
        assert!(req.validate().is_ok());
    }
}