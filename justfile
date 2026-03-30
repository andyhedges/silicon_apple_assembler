# ARM64 Sandbox API — build recipes

# Build the release binary
build:
    cargo build --release

# Run the release binary, forwarding any extra arguments
run *ARGS:
    cargo run --release -- {{ARGS}}

# Run all tests
test:
    cargo test