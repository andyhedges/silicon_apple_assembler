use crate::models::PipelineError;
use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tracing::{debug, error, info, warn};

/// Execution result
pub struct ExecutionResult {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: Option<i32>,
    pub killed_by_timeout: bool,
}

/// Internal state for the execution slot
struct SlotState {
    occupied: bool,
    started_at: Option<Instant>,
    timeout: Option<Duration>,
}

/// Global execution slot — ensures only one benchmark runs at a time.
/// Uses std::sync::Mutex so release in Drop is synchronous and deterministic.
pub struct ExecutionSlot {
    state: Mutex<SlotState>,
}

impl ExecutionSlot {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(SlotState {
                occupied: false,
                started_at: None,
                timeout: None,
            }),
        }
    }

    /// Try to acquire the execution slot. Returns Ok(guard) if acquired,
    /// or Err(retry_after_seconds) if the slot is busy.
    pub fn try_acquire(self: &Arc<Self>, timeout: Duration) -> Result<SlotGuard, u64> {
        let mut state = self.state.lock().unwrap();
        if state.occupied {
            let retry_after = match (state.started_at, state.timeout) {
                (Some(started), Some(t)) => {
                    let elapsed = started.elapsed();
                    let remaining = t.saturating_sub(elapsed);
                    std::cmp::max(1, remaining.as_secs())
                }
                _ => 1,
            };
            return Err(retry_after);
        }
        state.occupied = true;
        state.started_at = Some(Instant::now());
        state.timeout = Some(timeout);
        Ok(SlotGuard {
            slot: Arc::clone(self),
        })
    }
}

impl std::fmt::Debug for ExecutionSlot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExecutionSlot").finish()
    }
}

#[derive(Debug)]
pub struct SlotGuard {
    slot: Arc<ExecutionSlot>,
}

impl Drop for SlotGuard {
    fn drop(&mut self) {
        let mut state = self.slot.state.lock().unwrap();
        state.occupied = false;
        state.started_at = None;
        state.timeout = None;
    }
}

/// Generate the sandbox-exec profile for a specific job.
///
/// The profile starts with `(deny default)` which blocks everything, then
/// selectively allows only what the sandboxed binary needs: reading its own
/// binary, the sandbox profile file, system libraries, and the dyld shared
/// cache; writing to stdout/stderr/null. No network, no file creation.
fn generate_sandbox_profile(profile_path: &Path, temp_dir: &Path) -> String {
    format!(
        r#"(version 1)
(deny default)

;; Allow the process to execute
(allow process-exec)

;; Allow reading the binary, sandbox profile, temp dir, system libraries, and dyld cache
(allow file-read*
    (subpath "/usr/lib")
    (subpath "/System/Library")
    (subpath "/Library/Apple/usr/lib")
    (subpath "/private/var/db/dyld")
    (subpath "{temp_dir}")
    (literal "{profile}")
    (literal "/dev/urandom")
    (literal "/dev/null")
)

;; Allow writing to stdout, stderr, and null
(allow file-write*
    (literal "/dev/stdout")
    (literal "/dev/stderr")
    (literal "/dev/null")
)

;; Allow sysctl reads (needed by some system libraries during init)
(allow sysctl-read)

;; Allow mach lookups needed for basic process operation on macOS
(allow mach-lookup)
"#,
        temp_dir = temp_dir.display(),
        profile = profile_path.display()
    )
}

/// Execute a compiled binary in the sandbox with resource limits and wall-clock timeout.
pub async fn execute(
    job_id: &str,
    binary_path: &Path,
    temp_dir: &Path,
    timeout_seconds: u64,
) -> Result<ExecutionResult, PipelineError> {
    let job_id = job_id.to_string();
    let binary_path = binary_path.to_owned();
    let temp_dir = temp_dir.to_owned();

    let profile_path = temp_dir.join("sandbox-profile.sb");
    let profile_content = generate_sandbox_profile(&profile_path, &temp_dir);
    std::fs::write(&profile_path, &profile_content).map_err(|e| PipelineError::ServerError {
        message: format!("Failed to write sandbox profile: {}", e),
    })?;

    let timeout = Duration::from_secs(timeout_seconds);

    tokio::task::spawn_blocking(move || {
        execute_sandboxed(&job_id, &binary_path, &profile_path, timeout)
    })
    .await
    .map_err(|e| PipelineError::ServerError {
        message: format!("Execution task panicked: {}", e),
    })?
}

fn execute_sandboxed(
    job_id: &str,
    binary_path: &Path,
    profile_path: &Path,
    timeout: Duration,
) -> Result<ExecutionResult, PipelineError> {
    info!(job_id = job_id, "Starting sandboxed execution");

    // Shell wrapper sets resource limits before exec.
    //
    // Resource limits applied:
    // - ulimit -t 30: CPU time ceiling (30 seconds absolute; per-request timeout is lower)
    // - ulimit -c 0: no core dumps
    //
    // Limits NOT applied (and why):
    // - ulimit -v (virtual memory): On macOS, the dyld shared cache is memory-mapped
    //   at ~1-2 GB of virtual address space in every process. Setting ulimit -v to
    //   128 MB causes immediate process death before main() even runs. Memory is
    //   bounded by the sandbox profile (no mmap of new files) and the CPU time limit.
    // - ulimit -f (file size): sandbox-exec's deny-default profile prevents file
    //   creation. Setting ulimit -f 0 can interfere with sandbox-exec's own operation.
    // - ulimit -u (max processes): The shell needs to exec the sandboxed binary.
    //   The sandbox profile prevents fork/exec of child processes.
    // - ulimit -n (open files): Set to 32 to allow dyld to open shared libraries
    //   during process startup while still bounding resource usage.
    let script = format!(
        r#"
ulimit -t 30
ulimit -c 0
ulimit -n 32
exec sandbox-exec -f "{profile}" "{binary}"
"#,
        profile = profile_path.display(),
        binary = binary_path.display()
    );

    let start = Instant::now();

    let mut child = Command::new("bash")
        .args(["-c", &script])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| {
            debug!(
                job_id = job_id,
                error = %e,
                "Failed to spawn sandbox process"
            );
            PipelineError::ServerError {
                message: format!("Failed to spawn sandbox process: {}", e),
            }
        })?;

    // Wall-clock timeout enforcement via polling loop
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let stdout = read_pipe(child.stdout.take());
                let stderr = read_pipe(child.stderr.take());
                let exit_code = status.code();

                info!(
                    job_id = job_id,
                    exit_code = ?exit_code,
                    elapsed_ms = start.elapsed().as_millis() as u64,
                    "Process exited"
                );

                #[cfg(unix)]
                {
                    use std::os::unix::process::ExitStatusExt;
                    if let Some(_signal) = status.signal() {
                        return Ok(ExecutionResult {
                            stdout,
                            stderr,
                            exit_code: None,
                            killed_by_timeout: false,
                        });
                    }
                }

                return Ok(ExecutionResult {
                    stdout,
                    stderr,
                    exit_code,
                    killed_by_timeout: false,
                });
            }
            Ok(None) => {
                if start.elapsed() > timeout {
                    warn!(job_id = job_id, "Process exceeded timeout, sending SIGKILL");
                    let _ = child.kill();
                    let _ = child.wait();
                    let stdout = read_pipe(child.stdout.take());
                    return Ok(ExecutionResult {
                        stdout,
                        stderr: String::new(),
                        exit_code: None,
                        killed_by_timeout: true,
                    });
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(e) => {
                error!(job_id = job_id, error = %e, "Failed to wait for process");
                return Err(PipelineError::ServerError {
                    message: format!("Failed to wait for process: {}", e),
                });
            }
        }
    }
}

/// Read all content from an optional piped stream
fn read_pipe(pipe: Option<impl std::io::Read>) -> String {
    pipe.map(|mut s| {
        let mut buf = String::new();
        std::io::Read::read_to_string(&mut s, &mut buf).unwrap_or(0);
        buf
    })
    .unwrap_or_default()
}