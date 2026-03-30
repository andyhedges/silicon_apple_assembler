# ARM64 Assembly Sandbox & Benchmarking API

## Specification v1.1

---

## 1. Overview

### 1.1 Purpose

A web API that accepts user-submitted ARM64v8 (AArch64) assembly source code, compiles it, executes it in a restricted sandbox on Apple Silicon hardware, and returns the output and benchmark results.

### 1.2 Core Principles

- **Pure computation only**: User code may perform calculations and return results. It must not perform I/O, syscalls, or any privileged operations directly.
- **Defence in depth**: Security is enforced at four layers — static analysis, harness wrapping, kernel-level sandboxing, and resource limits.
- **Deterministic benchmarking**: Timing is performed by the harness, not the user code, to ensure consistency and prevent manipulation.

### 1.3 Target Platform

- Apple Silicon (M1/M2/M3/M4)
- macOS (latest stable release)
- Native AArch64 toolchain (Xcode Command Line Tools)

---

## 2. Architecture

```
┌──────────────────────────────────────────────────────────┐
│  Client                                                  │
│  POST /run  { source, entrypoint?, inputs?, iterations? }│
└──────────────────┬───────────────────────────────────────┘
                   │
                   ▼
┌──────────────────────────────────────────────────────────┐
│  API Server (e.g. Rust / Go / Python)                    │
│                                                          │
│  1. Validate request                                     │
│  2. Static analysis of assembly source                   │
│  3. Write source to temp directory                       │
│  4. Assemble & link with harness                         │
│  5. Execute in sandbox with resource limits              │
│  6. Capture stdout, stderr, exit code, timing            │
│  7. Clean up temp directory                              │
│  8. Return response                                      │
└──────────────────────────────────────────────────────────┘
```

### 2.1 Implementation Requirements

| Concern         | Decision                                                              |
| --------------- | --------------------------------------------------------------------- |
| **Language**    | Rust (stable toolchain)                                               |
| **Build system**| [`just`](https://github.com/casey/just) — `just build`, `just run`   |
| **Port**        | `--port <u16>`, default `80`                                          |
| **Logging**     | Structured logs to stdout; no log files                               |

#### `just` Recipes

```
just build   # cargo build --release
just run     # cargo run --release -- [args]
just test    # cargo test
```

`just run` should forward any extra arguments to the binary, so `just run --port 8080` works as expected.

#### Test Coverage

Tests are run with `just test`. The following areas must have test coverage:

| Area                        | What to test                                                                 |
| --------------------------- | ---------------------------------------------------------------------------- |
| **Static analyser**         | Each forbidden instruction and pattern triggers rejection; allowed constructs are accepted; correct line number reported in error |
| **Harness wire format**     | Parser correctly separates harness metadata from user stdout; malformed/missing header returns `RUNTIME_ERROR` |
| **Request validation**      | Missing required fields, out-of-range `iterations`/`timeout_seconds`, invalid `entrypoint` label, oversized source |
| **Rate limiting**           | Per-key limits are enforced; `429` is returned with correct headers          |
| **Execution slot**          | A second execution request while one is running returns `503` with a valid `Retry-After` value |
| **Timeout enforcement**     | A job that exceeds `timeout_seconds` is killed and returns `TIMEOUT`         |
| **End-to-end (integration)**| The example from §12 assembles, executes, and returns the correct `return_value` |

Unit tests live alongside the code (`#[cfg(test)]` modules). Integration tests that invoke the full pipeline (assemble → execute) live in `tests/`. Tests that require the Xcode toolchain or `sandbox-exec` should be gated with `#[ignore]` and documented, so the unit test suite passes in CI environments without the full toolchain.

#### CLI

```
USAGE:
    arm64-sandbox [OPTIONS]

OPTIONS:
    --port <PORT>    Port to listen on [default: 80]
    -h, --help       Print help
    -V, --version    Print version
```

#### README

A `README.md` must be present at the repository root. It must cover:

- **What it is** — a short description of the project
- **Requirements** — Rust toolchain version, Xcode Command Line Tools, `just`
- **Building** — `just build`
- **Running** — `just run`, the `--port` flag, example invocation
- **Testing** — `just test`, note on which tests require the full toolchain
- **API usage** — at minimum, the `POST /run` request/response example from §12
- **Security model** — a brief summary of the sandbox layers (§10)

The README is the primary entry point for new contributors and operators. It should be kept in sync with the spec as the implementation evolves.

#### OpenAPI Specification

An OpenAPI 3.1 spec must be present at `openapi.yaml` in the repository root. It must cover all endpoints, request/response schemas, error codes, and the `Authorization` header. The spec should be kept in sync with the implementation — it is the authoritative contract for API consumers. The README should reference it.

#### Logging

All log output goes to stdout in a structured format (e.g. JSON or logfmt). Each log line should include at minimum: timestamp, log level, and message. Per-request log lines should include the job ID so execution pipeline stages can be correlated. There is no log file, log rotation, or syslog integration — the process is expected to run under a supervisor (e.g. launchd) that handles log capture.

---

### 2.2 Component Summary

| Component              | Responsibility                                              |
| ---------------------- | ----------------------------------------------------------- |
| **API Server**         | HTTP interface, request validation, orchestration           |
| **Static Analyser**    | Rejects assembly containing forbidden instructions/patterns |
| **Harness**            | Wraps user code; provides entry, exit, stdout, timing       |
| **Compiler Pipeline**  | Assembles and links user code with harness                  |
| **Sandbox Executor**   | Runs binary under macOS sandbox-exec with resource limits   |
| **Benchmark Timer**    | Measures execution time across N iterations                 |

---

## 3. API Interface

### 3.1 Authentication

All requests must include a valid API key as a Bearer token:

```
Authorization: Bearer <api-key>
```

Requests without a valid key return `401 Unauthorized`. API key issuance and management (creation, rotation, revocation) are out of scope for this specification and handled by the platform's identity layer.

### 3.2 `POST /run`

#### Request Body (JSON)

```json
{
  "source": "string, required — ARM64 assembly source code",
  "entrypoint": "string, optional — label name the harness calls (default: '_user_entry')",
  "inputs": {
    "x0": 42,
    "x1": 17
  },
  "iterations": 1000,
  "timeout_seconds": 10
}
```

| Field              | Type              | Required | Default         | Constraints                    |
| ------------------ | ----------------- | -------- | --------------- | ------------------------------ |
| `source`           | string            | yes      | —               | Max 64 KB                      |
| `entrypoint`       | string            | no       | `_user_entry`   | Must match `/^_[a-zA-Z_]\w*$/` |
| `inputs`           | map<string, i64>  | no       | all zero        | Keys: x0–x7 only              |
| `iterations`       | integer           | no       | 1               | Range: 1–1,000,000            |
| `timeout_seconds`  | integer           | no       | 10              | Range: 1–30                   |

> **Note on `entrypoint`:** The leading underscore is required. On macOS, the linker prefixes all user-defined symbols with `_`; the API uses the post-link symbol name. A label `foo` in assembly becomes the symbol `_foo` after linking.

> **Note on `inputs`:** Only general-purpose integer registers x0–x7 are supported in v1. Vector/SIMD register inputs are a planned extension (see §11.4).

#### Success Response — `200 OK`

```json
{
  "status": "ok",
  "output": {
    "stdout": "string — captured standard output from harness print helpers",
    "return_value": 123,
    "registers": {
      "x0": 123
    }
  },
  "benchmark": {
    "iterations": 1000,
    "total_ns": 4820000,
    "mean_ns": 4820,
    "median_ns": 4790,
    "min_ns": 4610,
    "max_ns": 6200,
    "stddev_ns": 180
  }
}
```

#### Runtime Error Response — `200 OK`

When user code fails at runtime (crash or timeout), the API returns HTTP 200 with a non-`ok` status. The HTTP status reflects whether the API itself functioned correctly; the `status` field distinguishes user-code success from failure.

```json
{
  "status": "error",
  "error_code": "RUNTIME_ERROR",
  "message": "Process terminated with signal 11 (SIGSEGV)",
  "output": {
    "stdout": ""
  }
}
```

#### Compile/Analysis Error Response — `400 Bad Request`

```json
{
  "status": "error",
  "error_code": "STATIC_ANALYSIS_FAILED | COMPILE_ERROR | BINARY_VERIFICATION_FAILED",
  "message": "Human-readable explanation",
  "detail": {
    "line": 14,
    "instruction": "svc"
  }
}
```

#### Error Response — `429 Too Many Requests`

Standard rate-limit response. See §7.

#### Error Response — `503 Service Unavailable`

Returned immediately when the execution slot is occupied. The client should retry after the indicated delay.

```
HTTP/1.1 503 Service Unavailable
Retry-After: 4
Content-Type: application/json

{
  "status": "error",
  "error_code": "EXECUTION_SLOT_BUSY",
  "message": "A benchmark is currently running. Retry in 4 seconds.",
  "retry_after_seconds": 4
}
```

`retry_after_seconds` is computed as `max(1, job_deadline - now)` where `job_deadline = started_at + timeout_seconds` of the running job. This is a conservative upper bound — the slot may free sooner if the job finishes early.

---

## 4. Static Analysis

### 4.1 Purpose

Reject source code that contains instructions which could escape the sandbox or perform privileged operations. This is a fast-fail layer — the kernel sandbox (§6) is the true enforcement boundary.

### 4.2 Forbidden Instructions

The following instructions MUST cause immediate rejection:

| Instruction       | Reason                                     |
| ----------------- | ------------------------------------------ |
| `svc`             | Supervisor call (syscall)                  |
| `hvc`             | Hypervisor call                            |
| `smc`             | Secure monitor call                        |
| `eret`            | Exception return                           |
| `brk`             | Software breakpoint                        |
| `hlt`             | Halt                                       |
| `dcps1/2/3`       | Debug exception                            |
| `mrs`             | Read system register                       |
| `msr`             | Write system register                      |
| `sys`             | System instruction                         |
| `sysl`            | System instruction with result             |
| `dc`, `ic`, `at`  | Cache/TLB maintenance (privileged forms)   |
| `tlbi`            | TLB invalidate                             |

### 4.3 Restricted Patterns

| Pattern                          | Policy                                            |
| -------------------------------- | ------------------------------------------------- |
| Indirect branches (`br Xn`, `blr Xn`) | **Forbidden by default.** See §4.5 for rationale. |
| `.byte` / `.word` / `.long` / `.quad` in `.text` | **Forbidden.** Prevents encoding hidden instructions as raw data in the executable code section. Allowed in `.data` and `.rodata` sections only. |
| Assembler directives `.include`, `.incbin` | **Forbidden.** No external file references.   |
| Assembler macro directives `.macro`, `.endmacro` | **Forbidden.** Macros can expand to forbidden instructions that would not be caught by token-level scanning of the source text. |
| Labels starting with `_harness_` | **Reserved namespace.** User code must not define labels with this prefix. |
| More than 1,000 labels           | **Rejected.** Bounds symbol table complexity.     |

### 4.4 Allowed Constructs

Everything else is allowed, including:

- All general-purpose arithmetic, logic, shift, bitfield instructions
- All SIMD/NEON/FP instructions (computation only)
- Direct branches: `b`, `b.cond`, `bl`, `cbz`, `cbnz`, `tbz`, `tbnz`
- `ret` (return via link register)
- Stack operations: `stp`, `ldp`, `str`, `ldr` (within the allocated user stack region)
- Multiple user-defined functions called via `bl _label`
- `.data` and `.rodata` sections with arbitrary constant data
- `.align` directives

### 4.5 Rationale: Banning Indirect Branches

Indirect branches (`br Xn`, `blr Xn`) allow jumping to arbitrary addresses, which could target gadgets outside user code (e.g., libc syscall wrappers). Banning them eliminates this class of attack with minimal impact — direct `bl _label` calls support all typical benchmark patterns including recursion and multi-function programs. If a future use case requires function pointers (e.g., jump tables for interpreters), this can be relaxed with post-compilation binary verification (see §11 — Future Extensions).

### 4.6 Implementation Notes

- Analysis operates on the raw source text, not the binary.
- Strip comments (`//` and `/* */` and `;`) before scanning.
- Match instructions case-insensitively.
- Match against whole tokens to avoid false positives (e.g., `msr` inside a label name like `_my_msr_counter` should not trigger — scan only the instruction mnemonic position).
- `.macro` and `.endmacro` are forbidden outright (§4.3); do not attempt to expand macros before scanning.
- The analyser MUST return the offending line number and instruction in the error response.

---

## 5. Harness

### 5.1 Purpose

The harness is a trusted assembly file controlled by the API. It provides:

- Program entry (`_main`)
- A controlled stack for user code
- Input register setup (from API request)
- Per-iteration timing instrumentation
- A stdout print routine for outputting results
- Clean program exit

User code is assembled as a separate object file and linked with the harness. The user NEVER controls `_main` or any syscall instruction.

### 5.2 Harness Pseudocode

```
_main:
    // 1. Set up the user stack
    load address of _user_stack_top into sp

    // 2. Load user inputs into x0–x7 from embedded constants
    //    (patched at link time or written into .data by the build step)

    // 3. Warm-up phase: run first <warmup_count> iterations without timing
    //    warmup_count = max(1, iterations / 10)  if iterations > 10, else 0
    mov x21, #<warmup_count>
  _warmup_loop:
    cbz x21, _warmup_done
    // reload inputs into x0–x7
    bl _user_entry
    sub x21, x21, #1
    b _warmup_loop
  _warmup_done:

    // 4. Timed phase: measure each iteration individually
    mov x21, #<measured_count>      // iterations - warmup_count
    adr x23, _timing_buffer         // pointer into per-iteration tick buffer
  _bench_loop:
    // reload inputs into x0–x7 each iteration
    mrs x20, cntvct_el0             // start tick  (harness-only; user code cannot use mrs)
    bl _user_entry
    mrs x22, cntvct_el0             // end tick
    sub x22, x22, x20               // elapsed ticks for this iteration
    str x22, [x23], #8              // store in buffer; advance pointer
    sub x21, x21, #1
    cbnz x21, _bench_loop

    // x0 now holds the return value from the final measured iteration

    // 5. Compute summary statistics (min, max, mean, median, stddev)
    //    from _timing_buffer in-process

    // 6. Encode results to stdout using harness wire format (see §5.6)

    // 7. Exit
    mov x16, #1
    mov x0, #0
    svc #0x80
```

### 5.3 Stdout Helper

The harness provides a `_harness_print_u64` function that the user MAY call via `bl _harness_print_u64` to print the unsigned 64-bit value in `x0` as a decimal string followed by a newline. This is the **only** I/O mechanism available to user code.

Additional helpers:

| Function                  | Behaviour                                            |
| ------------------------- | ---------------------------------------------------- |
| `_harness_print_u64`      | Print x0 as unsigned decimal + newline               |
| `_harness_print_i64`      | Print x0 as signed decimal + newline                 |
| `_harness_print_hex`      | Print x0 as `0x`-prefixed hex + newline              |
| `_harness_print_char`     | Print low byte of x0 as ASCII character              |
| `_harness_print_newline`  | Print a newline                                      |

All helpers preserve registers x1–x30 (caller's registers are not clobbered).

> **Benchmarking note:** Each print helper call performs a `write` syscall. If user code calls print helpers on every iteration of a hot loop, the syscall overhead will dominate timing results. Print helpers are intended for reporting final results after the computation completes, not for per-iteration output. The harness does not suppress user I/O during timed iterations — any I/O the user code performs will be included in the measured time.

### 5.4 User Stack

- Size: 64 KB (configurable at build time)
- Allocated in the harness `.bss` section
- `sp` is set to the top of this region before calling user code
- Stack overflow will fault into the sandbox, which terminates the process

### 5.5 Build Pipeline

```bash
# 1. Write user source to temp file
echo "$USER_SOURCE" > /tmp/job_xxxx/user_code.s

# 2. Assemble user code
as -o /tmp/job_xxxx/user_code.o /tmp/job_xxxx/user_code.s

# 3. Assemble harness (pre-built, or generated per-request with patched inputs)
as -o /tmp/job_xxxx/harness.o /tmp/job_xxxx/harness.s

# 4. Link
ld -o /tmp/job_xxxx/program \
   /tmp/job_xxxx/harness.o \
   /tmp/job_xxxx/user_code.o \
   -lSystem \
   -syslibroot $(xcrun --sdk macosx --show-sdk-path) \
   -e _main \
   -arch arm64

# 5. (Optional) Post-link verification — disassemble and re-check
otool -tv /tmp/job_xxxx/program | analyse_binary
```

### 5.6 Harness Output Wire Format

The harness writes all structured output (timing statistics and return value) as a single header line to stdout, followed immediately by any user-generated output. This allows the API server to unambiguously separate harness metadata from user stdout.

**Format:**

```
HARNESS:rv=<i64>;n=<u64>;freq=24000000;total=<u64>;mean=<u64>;median=<u64>;min=<u64>;max=<u64>;stddev=<u64>\n
<user stdout — zero or more lines>
```

- All timing fields are in timer ticks. The API server converts to nanoseconds: `ns = ticks * 1_000_000_000 / freq`.
- `rv` is the return value from `x0` after the final measured iteration, as a signed 64-bit decimal.
- `n` is the number of measured iterations (total requested minus warm-up).
- `freq` is always `24000000` on Apple Silicon (see §8.1). It is included in the wire format for documentation and forward-compatibility.
- The harness line always appears first; the API server MUST read and strip it before exposing `stdout` to the caller.
- If the process exits before writing the harness line (crash, timeout), the API returns `RUNTIME_ERROR` or `TIMEOUT`.

### 5.7 Post-Link Binary Verification (Recommended)

After linking, disassemble the final binary and scan for any forbidden instruction opcodes. This catches edge cases such as:

- Instructions smuggled via `.byte` directives in data sections that the linker placed in an executable segment
- Toolchain-inserted stubs or trampolines

Use `otool -tv` or a custom disassembler to verify. Reject the binary if any instruction from §4.2 appears outside the harness's own code.

---

## 6. Sandbox Execution

### 6.1 macOS Sandbox Profile (sandbox-exec)

> **Deprecation notice:** `sandbox-exec` has been deprecated since macOS 10.15. It remains functional on current releases but may be removed in a future version. Treat it as load-bearing infrastructure that must be re-evaluated with each macOS release. If it is removed, the sandbox layer will need to be replaced — likely with a launchd-managed sandbox or a Linux ARM64 container (see §11.5).

```scheme
;; sandbox-profile.sb  (path templated per job before exec)
(version 1)
(deny default)

;; Allow the process to execute (required for dyld initialisation)
(allow process-exec)

;; Allow reading the binary, system libraries, and dyld shared cache
(allow file-read*
    (subpath "/usr/lib")
    (subpath "/System/Library")
    (literal "/tmp/job_xxxx/program")    ;; templated per job
)

;; Allow writing to pre-opened stdout, stderr, and null only
(allow file-write*
    (literal "/dev/stdout")
    (literal "/dev/stderr")
    (literal "/dev/null")
)
```

The `(deny default)` rule at the top denies all operations not explicitly allowed above. **Do not add explicit `(deny ...)` rules after the allow rules.** In SBPL, the last matching rule wins, so a trailing `(deny file-write*)` would override `(allow file-write* (literal "/dev/stdout"))` and prevent stdout writes. The default deny is sufficient — targeted allows are the only additions needed.

### 6.2 Resource Limits

Applied via `setrlimit()` or shell `ulimit` before exec:

| Resource        | Limit      | Rationale                              |
| --------------- | ---------- | -------------------------------------- |
| CPU time        | 30 seconds | Absolute ceiling (per-request timeout is lower) |
| Virtual memory  | 128 MB     | Prevent excessive allocation           |
| File size       | 0 bytes    | No file creation                       |
| Open files      | 4          | stdin, stdout, stderr + 1 spare        |
| Processes       | 1          | No forking                             |
| Core dump size  | 0          | No core dumps                          |

### 6.3 Wall-Clock Timeout

The API server MUST enforce a wall-clock timeout independently of CPU limits. Use a parent process that:

1. Forks and execs the sandboxed binary
2. Starts a timer
3. Sends `SIGKILL` if the timer expires
4. Collects exit status and output

This protects against infinite loops in computation that don't exceed CPU time (e.g., tight loops with `yield`-like patterns).

### 6.4 Execution Flow

```
API Server
    │
    ├── Create temp directory /tmp/job_<uuid>/
    ├── Write user_code.s
    ├── Run static analyser → reject or continue
    ├── Assemble + link → reject on compile error or continue
    ├── (Optional) Post-link binary verification
    │
    ├── Fork child process:
    │       setrlimit(...)
    │       sandbox-exec -f sandbox-profile.sb ./program
    │
    ├── Parent: wait with timeout
    │       ├── Normal exit → capture stdout, exit code
    │       ├── Timeout → SIGKILL, return TIMEOUT error
    │       └── Signal (crash) → return RUNTIME_ERROR
    │
    ├── Parse harness wire format from stdout (§5.6)
    ├── Delete temp directory
    └── Return JSON response
```

---

## 7. Rate Limiting & Abuse Prevention

### 7.1 Rate Limits

| Scope                    | Limit               | Window   |
| ------------------------ | ------------------- | -------- |
| Per API key              | 60 requests         | 1 minute |
| Per API key              | 500 requests        | 1 hour   |
| Global — compile phase   | N concurrent jobs   | —        |
| Global — execution phase | **1 concurrent job**| —        |

The compile phase (static analysis, assemble, link, verify) can run concurrently for multiple jobs since it is purely file I/O and CPU work that does not affect benchmark timing. The execution phase is limited to **one job at a time** to prevent cache pollution and CPU contention from distorting benchmark results (see §8.4). `N` for the compile phase should be sized to match throughput — a value of 4–8 is reasonable.

### 7.2 Source Size Limits

| Field            | Max Size |
| ---------------- | -------- |
| `source`         | 64 KB    |
| Total functions  | No explicit limit (bounded by source size) |
| Label count      | 1,000 (enforced during static analysis — see §4.3) |

### 7.3 No Queueing

There is no server-side job queue. If the execution slot is occupied when a request reaches the execution phase, the server rejects it immediately with `503 Service Unavailable` and a `Retry-After` header (see §3.2). Clients are responsible for retrying. This keeps server state simple and gives clients accurate, actionable feedback rather than an unpredictable wait.

---

## 8. Benchmarking

### 8.1 Timing Method

The harness uses `mrs Xn, cntvct_el0` to read the virtual timer counter before and after each individual iteration. On all Apple Silicon variants (M1–M4), `cntvct_el0` runs at a fixed 24 MHz. The harness uses this constant directly (`freq = 24,000,000`) rather than reading `cntfrq_el0` at runtime, avoiding an unnecessary `mrs` instruction and a potential source of variation.

Conversion: `nanoseconds = ticks × 1,000,000,000 / 24,000,000`

### 8.2 Warm-Up

- If `iterations > 10`, the first 10% of iterations (minimum 1) are treated as warm-up and excluded from all reported statistics, including `total_ns`.
- If `iterations <= 10`, no warm-up is applied.

### 8.3 Reported Statistics

The harness computes summary statistics from the per-iteration tick buffer in-process and encodes them in the wire format (§5.6). The API server converts ticks to nanoseconds and returns:

| Metric        | Description                                              |
| ------------- | -------------------------------------------------------- |
| `total_ns`    | Sum of all measured iterations (warm-up excluded)        |
| `mean_ns`     | Arithmetic mean per measured iteration                   |
| `median_ns`   | Median per measured iteration                            |
| `min_ns`      | Fastest measured iteration                               |
| `max_ns`      | Slowest measured iteration                               |
| `stddev_ns`   | Standard deviation across measured iterations            |

### 8.4 Isolation for Consistent Results

- Attempt to prefer a performance core (P-core) using `thread_policy_set` with `THREAD_AFFINITY_POLICY` or `taskpolicy`. On macOS, thread affinity is advisory — the OS scheduler may not honour it, and P-core placement cannot be guaranteed. `stddev_ns` reflects this non-determinism and is the primary indicator of result consistency.
- Run benchmarks at the highest QoS class (`DISPATCH_QUEUE_PRIORITY_HIGH` / `nice -n -20` if permitted).
- Accept that macOS is not a real-time OS — report `stddev_ns` so users can judge consistency.

---

## 9. Error Handling

### 9.1 Error Codes

| Code                      | HTTP Status | Cause                                              |
| ------------------------- | ----------- | -------------------------------------------------- |
| `STATIC_ANALYSIS_FAILED`  | 400         | Source contains forbidden instruction or pattern    |
| `COMPILE_ERROR`           | 400         | Assembler or linker error                           |
| `BINARY_VERIFICATION_FAILED` | 400      | Post-link scan found forbidden opcodes              |
| `RUNTIME_ERROR`           | 200*        | Process crashed (segfault, bus error, illegal instr)|
| `TIMEOUT`                 | 200*        | Process exceeded wall-clock or CPU time limit       |
| `EXECUTION_SLOT_BUSY`     | 503         | Execution slot occupied; retry after indicated delay|
| `RATE_LIMITED`            | 429         | Too many requests                                   |
| `SERVER_ERROR`            | 500         | Internal failure                                    |

*Runtime errors and timeouts return 200 because the API functioned correctly — the user's code simply failed. The `status` field distinguishes success from failure. See §3.2 for the runtime error response schema.

### 9.2 Compile Error Detail

When the assembler or linker fails, include the raw error output (with temp paths stripped) in the `message` field so the user can debug their code.

---

## 10. Security Summary

| Layer                      | Enforces                                                | Bypass Difficulty |
| -------------------------- | ------------------------------------------------------- | ----------------- |
| **Static analysis**        | No forbidden instructions in source                     | Low (can be creative) |
| **Harness wrapping**       | User code has no `_main`, no `svc`                      | Medium            |
| **Post-link verification** | No forbidden opcodes in final binary                    | High              |
| **sandbox-exec**           | Kernel denies file, network, IPC, fork, mach ports      | Very high         |
| **Resource limits**        | Bounded CPU, memory, no file creation                   | Very high         |
| **Wall-clock timeout**     | SIGKILL after N seconds                                 | Cannot bypass     |

Each layer independently prevents a class of abuse. An attacker must defeat ALL layers to cause harm.

---

## 11. Future Extensions

These are out of scope for v1 but should be considered in the design to avoid costly refactors.

### 11.1 Indirect Branch Support

If needed, allow `blr Xn` with post-compilation verification: disassemble the final binary, extract all possible branch targets from register-loading instructions, and verify they all land within the user code's `.text` section. This is complex but enables function pointers and computed gotos.

### 11.2 Shared Memory / Multi-Core Benchmarks

Allow benchmarking of atomic operations and memory ordering by running user code on multiple threads within the sandbox. Requires careful extension of the harness and security model.

### 11.3 Comparison Mode

Accept two source submissions and benchmark them head-to-head under identical conditions, returning a comparative analysis.

### 11.4 SIMD/NEON-Specific Benchmarking

Return additional NEON-specific metrics such as operations per cycle for vectorised code. Also enable vector register inputs (v0–v31) in the `inputs` field.

### 11.5 Linux Cross-Compilation

Support Linux AArch64 assembly (different syscall ABI) by compiling and running inside a Linux ARM64 Docker container on the same Apple Silicon host. This would use `seccomp-bpf` instead of `sandbox-exec` for the sandbox layer.

### 11.6 Persistent Code Storage

Allow users to save, version, and share assembly snippets via the API with unique identifiers.

---

## 12. Example: Full Request/Response Cycle

### Request

```json
{
  "source": ".global _user_entry\n.align 2\n\n_user_entry:\n    stp x29, x30, [sp, #-16]!\n    mov x29, sp\n    mov x2, #0\n    mov x3, x0\n_loop:\n    cbz x3, _done\n    add x2, x2, x3\n    sub x3, x3, #1\n    b _loop\n_done:\n    mov x0, x2\n    ldp x29, x30, [sp], #16\n    ret\n",
  "inputs": { "x0": 100 },
  "iterations": 10000,
  "timeout_seconds": 5
}
```

This computes the sum 1 + 2 + ... + 100.

### Response

```json
{
  "status": "ok",
  "output": {
    "stdout": "",
    "return_value": 5050,
    "registers": {
      "x0": 5050
    }
  },
  "benchmark": {
    "iterations": 10000,
    "total_ns": 1230000,
    "mean_ns": 123,
    "median_ns": 121,
    "min_ns": 118,
    "max_ns": 302,
    "stddev_ns": 11
  }
}
```

---

## 13. Glossary

| Term              | Definition                                                                 |
| ----------------- | -------------------------------------------------------------------------- |
| **Harness**       | Trusted assembly wrapper that provides entry, exit, timing, and I/O       |
| **User code**     | Untrusted assembly submitted via the API                                  |
| **Pure function** | A function with no side effects — takes register inputs, returns a value  |
| **sandbox-exec**  | macOS command-line tool for running a process under a Seatbelt sandbox    |
| **svc**           | ARM64 supervisor call instruction — the mechanism for making syscalls     |
| **P-core**        | Performance core on Apple Silicon (as opposed to E-core / efficiency core)|
