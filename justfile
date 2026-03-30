# ARM64 Sandbox API — build recipes

# Build the release binary
build:
    cargo build --release

# Run the release binary, forwarding any extra arguments
run *ARGS:
    cargo run --release -- {{ARGS}}

# Run tests — on macOS, includes ignored tests that need the native toolchain
test:
    #!/usr/bin/env bash
    if [[ "$(uname -s)" == "Darwin" ]]; then
        echo "Detected macOS — running all tests including toolchain-dependent tests"
        cargo test -- --include-ignored
    else
        echo "Non-macOS platform — running portable tests only (use 'just test-all' to force all)"
        cargo test
    fi

# Run all tests unconditionally, including those gated with #[ignore]
test-all:
    cargo test -- --include-ignored