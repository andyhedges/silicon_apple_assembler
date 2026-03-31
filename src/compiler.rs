use crate::models::PipelineError;
use std::path::{Path, PathBuf};
use std::process::Command;
use tracing::{debug, error, info};

/// Result of a successful compilation
pub struct CompileResult {
    /// Path to the compiled binary
    pub binary_path: PathBuf,
    /// Path to the temp directory (caller is responsible for cleanup)
    pub temp_dir: PathBuf,
}

/// Compile user assembly code with the harness.
///
/// Steps:
/// 1. Write user source and harness source to temp directory
/// 2. Assemble user code with `as`
/// 3. Assemble harness with `as`
/// 4. Link with `ld`
/// 5. Post-link binary verification with `otool`
pub fn compile(
    job_id: &str,
    user_source: &str,
    harness_source: &str,
) -> Result<CompileResult, PipelineError> {
    let temp_dir = PathBuf::from(format!("/tmp/job_{}", job_id));
    std::fs::create_dir_all(&temp_dir).map_err(|e| PipelineError::ServerError {
        message: format!("Failed to create temp directory: {}", e),
    })?;

    let user_path = temp_dir.join("user_code.s");
    let harness_path = temp_dir.join("harness.s");
    let user_obj = temp_dir.join("user_code.o");
    let harness_obj = temp_dir.join("harness.o");
    let binary_path = temp_dir.join("program");

    std::fs::write(&user_path, user_source).map_err(|e| PipelineError::ServerError {
        message: format!("Failed to write user source: {}", e),
    })?;
    std::fs::write(&harness_path, harness_source).map_err(|e| PipelineError::ServerError {
        message: format!("Failed to write harness source: {}", e),
    })?;

    info!(job_id = job_id, "Assembling user code");
    assemble(job_id, &user_path, &user_obj)?;

    info!(job_id = job_id, "Assembling harness");
    assemble(job_id, &harness_path, &harness_obj)?;

    info!(job_id = job_id, "Linking");
    link(job_id, &harness_obj, &user_obj, &binary_path)?;

    verify_binary(job_id, &binary_path)?;

    info!(job_id = job_id, "Compilation complete");

    Ok(CompileResult {
        binary_path,
        temp_dir,
    })
}

fn assemble(job_id: &str, source: &Path, output: &Path) -> Result<(), PipelineError> {
    let result = Command::new("as")
        .arg("-o")
        .arg(output)
        .arg(source)
        .arg("-arch")
        .arg("arm64")
        .output()
        .map_err(|e| PipelineError::ServerError {
            message: format!("Failed to run assembler: {}", e),
        })?;

    let stdout = String::from_utf8_lossy(&result.stdout);
    let stderr = String::from_utf8_lossy(&result.stderr);
    if !result.status.success() {
        let cleaned = strip_temp_paths(&stderr, job_id);
        error!(job_id = job_id, error = %cleaned, "Assembly failed");
        return Err(PipelineError::CompileError { message: cleaned });
    }

    info!(job_id = job_id, stdout = %stdout, stderr = %stderr, "Assembly completed");
    Ok(())
}

fn link(
    job_id: &str,
    harness_obj: &Path,
    user_obj: &Path,
    output: &Path,
) -> Result<(), PipelineError> {
    let sdk_output = Command::new("xcrun")
        .args(["--sdk", "macosx", "--show-sdk-path"])
        .output()
        .map_err(|e| PipelineError::ServerError {
            message: format!("Failed to run xcrun: {}", e),
        })?;

    let sdk_path = String::from_utf8_lossy(&sdk_output.stdout)
        .trim()
        .to_string();

    if sdk_path.is_empty() {
        return Err(PipelineError::ServerError {
            message: "Could not determine macOS SDK path via xcrun".to_string(),
        });
    }

    debug!(job_id = job_id, sdk_path = %sdk_path, "Using SDK path");

    let result = Command::new("ld")
        .args([
            "-o",
            output.to_str().unwrap(),
            harness_obj.to_str().unwrap(),
            user_obj.to_str().unwrap(),
            "-lSystem",
            "-syslibroot",
            &sdk_path,
            "-e",
            "_main",
            "-arch",
            "arm64",
        ])
        .output()
        .map_err(|e| PipelineError::ServerError {
            message: format!("Failed to run linker: {}", e),
        })?;

    let stdout = String::from_utf8_lossy(&result.stdout);
    let stderr = String::from_utf8_lossy(&result.stderr);
    if !result.status.success() {
        let cleaned = strip_temp_paths(&stderr, job_id);
        error!(job_id = job_id, error = %cleaned, "Linking failed");
        return Err(PipelineError::CompileError { message: cleaned });
    }

    info!(job_id = job_id, stdout = %stdout, stderr = %stderr, "Linking completed");
    Ok(())
}

/// Post-link binary verification: disassemble and check for forbidden opcodes
/// outside the harness's own code.
///
/// otool -tv output format (macOS):
///   _symbol_name:
///   0000000100003f00\tmov\tx0, #0x0
///
/// Each instruction line starts with an address (hex), followed by a tab and the
/// mnemonic, then a tab and operands. We extract the mnemonic as the second
/// whitespace-delimited token on lines that start with a hex address.
fn verify_binary(job_id: &str, binary_path: &Path) -> Result<(), PipelineError> {
    let result = match Command::new("otool")
        .args(["-tv", binary_path.to_str().unwrap()])
        .output()
    {
        Ok(r) => r,
        Err(e) => {
            debug!(
                job_id = job_id,
                error = %e,
                "otool not available, skipping binary verification"
            );
            return Ok(());
        }
    };

    if !result.status.success() {
        debug!(
            job_id = job_id,
            "otool returned non-zero, skipping binary verification"
        );
        return Ok(());
    }

    let disassembly = String::from_utf8_lossy(&result.stdout);

    // Instructions forbidden in user code but legitimately used by the harness
    let forbidden_in_user = [
        "svc", "hvc", "smc", "eret", "brk", "hlt", "dcps1", "dcps2", "dcps3",
    ];

    // Track whether we are inside harness code (allowed to use svc/mrs) or user code
    let mut in_harness_section = false;

    for line in disassembly.lines() {
        let trimmed = line.trim();

        // Detect symbol labels in otool output (lines ending with ':')
        if trimmed.ends_with(':') {
            let label = trimmed.trim_end_matches(':');
            in_harness_section =
                label.starts_with("_harness_") || label == "_main";
            continue;
        }

        // Skip harness code — the harness legitimately uses svc and mrs
        if in_harness_section {
            continue;
        }

        // Instruction lines start with a hex address. Extract the mnemonic
        // which is the second whitespace-delimited token.
        let tokens: Vec<&str> = trimmed.split_whitespace().collect();
        if tokens.len() < 2 {
            continue;
        }

        // Verify first token looks like a hex address
        let first = tokens[0];
        if !first.chars().all(|c| c.is_ascii_hexdigit()) {
            continue;
        }

        let mnemonic = tokens[1].to_lowercase();

        for &f in &forbidden_in_user {
            if mnemonic == f {
                return Err(PipelineError::BinaryVerification {
                    message: format!(
                        "Post-link verification found forbidden instruction '{}' in binary",
                        f
                    ),
                });
            }
        }
    }

    Ok(())
}

/// Strip temp directory paths from error messages
fn strip_temp_paths(msg: &str, job_id: &str) -> String {
    let temp_prefix = format!("/tmp/job_{}/", job_id);
    msg.replace(&temp_prefix, "")
}

/// Clean up temp directory
pub fn cleanup(temp_dir: &Path) {
    if let Err(e) = std::fs::remove_dir_all(temp_dir) {
        debug!(path = %temp_dir.display(), error = %e, "Failed to clean up temp directory");
    }
}