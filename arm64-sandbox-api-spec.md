# ARM64 Assembly Sandbox & Benchmarking API

## Specification v1.0

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

### 2.1 Component Summary

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

### 3.1 `POST /run`

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

#### Error Response — `400 Bad Request`

```json
{
  "status": "error",
  "error_code": "STATIC_ANALYSIS_FAILED | COMPILE_ERROR | RUNTIME_ERROR | TIMEOUT",
  "message": "Human-readable explanation",
  "detail": {
    "line": 14,
    "instruction": "svc"
  }
}
```

#### Error Response — `429 Too Many Requests`

Standard rate-limit response. See §7.

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
| Labels starting with `_harness_` | **Reserved namespace.** User code must not define labels with this prefix. |

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

Indirect branches (`br Xn`, `blr Xn`) allow jumping to arbitrary addresses, which could target gadgets outside user code (e.g., libc syscall wrappers). Banning them eliminates this class of attack with minimal impact — direct `bl _label` calls support all typical benchmark patterns including recursion and multi-function programs. If a future use case requires function pointers (e.g., jump tables for interpreters), this can be relaxed with post-compilation binary verification (see §10 — Future Extensions).

### 4.6 Implementation Notes

- Analysis operates on the raw source text, not the binary.
- Strip comments (`//` and `/* */` and `;`) before scanning.
- Match instructions case-insensitively.
- Match against whole tokens to avoid false positives (e.g., `msr` inside a label name like `_my_msr_counter` should not trigger — scan only the instruction mnemonic position).
- The analyser MUST return the offending line number and instruction in the error response.

---

## 5. Harness

### 5.1 Purpose

The harness is a trusted assembly file controlled by the API. It provides:

- Program entry (`_main`)
- A controlled stack for user code
- Input register setup (from API request)
- A call to the user's entry point
- Timing instrumentation
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

    // 3. Read cycle counter / wall clock (start)
    mrs x20, cntvct_el0       // virtual timer count (allowed in the harness)

    // 4. Loop: call user entry N times
    mov x21, #<iterations>
  _bench_loop:
    // reload inputs into x0–x7 each iteration
    bl _user_entry
    sub x21, x21, #1
    cbnz x21, _bench_loop

    // 5. Read cycle counter / wall clock (end)
    mrs x22, cntvct_el0

    // 6. Compute elapsed, store per-iteration results
    //    (write to a results buffer in .data)

    // 7. Print results to stdout
    //    (call _harness_print_x0, etc.)

    // 8. Exit
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

### 5.6 Post-Link Binary Verification (Recommended)

After linking, disassemble the final binary and scan for any forbidden instruction opcodes. This catches edge cases such as:

- Instructions smuggled via `.byte` directives in data sections that the linker placed in an executable segment
- Toolchain-inserted stubs or trampolines

Use `otool -tv` or a custom disassembler to verify. Reject the binary if any instruction from §4.2 appears outside the harness's own code.

---

## 6. Sandbox Execution

### 6.1 macOS Sandbox Profile (sandbox-exec)

```scheme
;; sandbox-profile.sb
(version 1)
(deny default)

;; Allow the process to execute
(allow process-exec)

;; Allow reading the binary itself and dyld shared cache
(allow file-read*
    (subpath "/usr/lib")
    (subpath "/System/Library")
    (literal "/tmp/job_xxxx/program")    ;; templated per job
)

;; Allow writing to stdout and stderr (pre-opened fds)
(allow file-write*
    (literal "/dev/stdout")
    (literal "/dev/stderr")
    (literal "/dev/null")
)

;; Deny everything else explicitly
(deny network*)
(deny file-write*)
(deny file-read*)
(deny ipc*)
(deny mach-lookup)
(deny signal)
(deny sysctl-write)
(deny process-fork)
(deny process-exec)
```

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
    ├── Parse harness output (timing data, return value)
    ├── Delete temp directory
    └── Return JSON response
```

---

## 7. Rate Limiting & Abuse Prevention

### 7.1 Rate Limits

| Scope          | Limit                  | Window   |
| -------------- | ---------------------- | -------- |
| Per API key    | 60 requests            | 1 minute |
| Per API key    | 500 requests           | 1 hour   |
| Global         | 200 concurrent jobs    | —        |

### 7.2 Source Size Limits

| Field            | Max Size |
| ---------------- | -------- |
| `source`         | 64 KB    |
| Total functions  | No explicit limit (bounded by source size) |
| Label count      | 1,000    |

### 7.3 Queueing

If concurrency limit is reached, requests are queued with a maximum wait time of 30 seconds. If the job cannot be started within that window, return `503 Service Unavailable`.

---

## 8. Benchmarking

### 8.1 Timing Method

The harness uses `mrs Xn, cntvct_el0` to read the virtual timer counter before and after each iteration. The counter frequency is read via `mrs Xn, cntfrq_el0` to convert ticks to nanoseconds.

### 8.2 Warm-Up

- If `iterations > 10`, the first 10% of iterations (minimum 1) are treated as warm-up and excluded from reported statistics.
- If `iterations <= 10`, no warm-up is applied.

### 8.3 Reported Statistics

For each benchmark run, the harness writes raw per-iteration tick counts to a buffer. The API server reads these from stdout (encoded by the harness) and computes:

| Metric        | Description                          |
| ------------- | ------------------------------------ |
| `total_ns`    | Wall-clock time for all iterations   |
| `mean_ns`     | Arithmetic mean per iteration        |
| `median_ns`   | Median per iteration                 |
| `min_ns`      | Fastest iteration                    |
| `max_ns`      | Slowest iteration                    |
| `stddev_ns`   | Standard deviation                   |

### 8.4 Isolation for Consistent Results

- Pin the process to a single performance core (P-core) using `thread_policy_set` with `THREAD_AFFINITY_POLICY` if available, or `taskpolicy -b` to avoid efficiency cores.
- Disable efficiency core migration where possible.
- Run benchmarks at the highest QoS class (`DISPATCH_QUEUE_PRIORITY_HIGH` / `nice -n -20` if permitted).
- Accept that macOS is not a real-time OS — report stddev so users can judge consistency.

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
| `RATE_LIMITED`            | 429         | Too many requests                                   |
| `SERVER_ERROR`            | 500         | Internal failure                                    |

*Runtime errors and timeouts return 200 because the API functioned correctly — the user's code simply failed. The `status` field distinguishes success from failure.

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

Return additional NEON-specific metrics such as operations per cycle for vectorised code.

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
