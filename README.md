# ARM64 Assembly Sandbox & Benchmarking API

A web API that accepts user-submitted ARM64v8 (AArch64) assembly source code, compiles it, executes it in a restricted sandbox on Apple Silicon hardware, and returns the output and benchmark results.

## Requirements

- **Rust** — stable toolchain (1.70+)
- **Xcode Command Line Tools** — provides `as`, `ld`, `otool`, `xcrun` for the native AArch64 toolchain
- **`just`** — command runner ([installation](https://github.com/casey/just#installation))
- **macOS on Apple Silicon** (M1/M2/M3/M4) — required for execution; compilation and unit tests work on any platform

### Installing Dependencies with mise

If you use [mise](https://mise.jdx.dev/), the included `mise.toml` will install the Rust toolchain and `just` automatically:

```bash
mise install
```

## Building

```bash
just build
```

This runs `cargo build --release`.

## Running

```bash
just run -- --bearer <your-secret-token> --port 8080
```

The `--bearer` flag is required and sets the API key that clients must provide in the `Authorization: Bearer <token>` header. Requests with a missing or non-matching token receive `401 Unauthorized`.

### CLI Options

```
USAGE:
    arm64-sandbox [OPTIONS] --bearer <TOKEN>

OPTIONS:
    --port <PORT>      Port to listen on [default: 80]
    --bearer <TOKEN>   Required Bearer token for API authentication
    -h, --help         Print help
    -V, --version      Print version
```

## Testing

```bash
just test
```

This runs `cargo test`, which executes all unit tests. Tests are organized as:

- **Unit tests** — inline `#[cfg(test)]` modules alongside each source file
- **Integration tests** — in `tests/` directory, requiring the full macOS toolchain

Tests that require Xcode Command Line Tools or `sandbox-exec` (i.e., only available on macOS with the native AArch64 toolchain) are gated with `#[ignore]` and will be skipped in CI environments. To run them locally on macOS:

```bash
cargo test -- --ignored
```

## API Usage

All requests require authentication via Bearer token (must match the `--bearer` value the server was started with):

```
Authorization: Bearer <token>
```

See [`openapi.yaml`](openapi.yaml) for the full API specification.

### `POST /run` — Example

**Request:**

```bash
curl -X POST http://localhost:8080/run \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer my-secret-token" \
  -d '{
    "source": ".global _user_entry\n.align 2\n\n_user_entry:\n    stp x29, x30, [sp, #-16]!\n    mov x29, sp\n    mov x2, #0\n    mov x3, x0\n_loop:\n    cbz x3, _done\n    add x2, x2, x3\n    sub x3, x3, #1\n    b _loop\n_done:\n    mov x0, x2\n    ldp x29, x30, [sp], #16\n    ret\n",
    "inputs": { "x0": 100 },
    "iterations": 10000,
    "timeout_seconds": 5
  }'
```

This computes the sum 1 + 2 + ... + 100.

**Response:**

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

### Error Responses

| HTTP Status | Error Code | Cause |
|-------------|-----------|-------|
| 400 | `STATIC_ANALYSIS_FAILED` | Source contains forbidden instruction or pattern |
| 400 | `COMPILE_ERROR` | Assembler or linker error |
| 400 | `BINARY_VERIFICATION_FAILED` | Post-link scan found forbidden opcodes |
| 200 | `RUNTIME_ERROR` | Process crashed (segfault, bus error, etc.) |
| 200 | `TIMEOUT` | Process exceeded wall-clock or CPU time limit |
| 401 | `UNAUTHORIZED` | Missing or invalid Bearer token |
| 429 | `RATE_LIMITED` | Too many requests |
| 503 | `EXECUTION_SLOT_BUSY` | Benchmark currently running; retry after indicated delay |

## Security Model

Security is enforced at multiple independent layers (defence in depth):

| Layer | Enforces |
|-------|----------|
| **Bearer token authentication** | Only clients with the configured token can access the API |
| **Static analysis** | Rejects source containing forbidden instructions (svc, mrs, msr, etc.) and restricted patterns (indirect branches, raw data in .text, macros) |
| **Harness wrapping** | User code has no `_main`, no syscall instructions; only the harness controls program entry, exit, and I/O |
| **Post-link binary verification** | Disassembles the final binary and scans for forbidden opcodes that may have been smuggled past source analysis |
| **sandbox-exec** | macOS kernel sandbox denies network access, file creation, process forking, and signal sending |
| **Resource limits** | Bounded CPU (30s), no core dumps, bounded file descriptors |
| **Wall-clock timeout** | Parent process sends SIGKILL after the configured timeout; cannot be bypassed |

Each layer independently prevents a class of abuse. An attacker must defeat all layers to cause harm.

## License

See repository for license information.