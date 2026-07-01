# ─── review-engine development tasks ────────────────────────────

# Build (debug, with CLI feature)
build:
    cargo build --features cli

# Build (release, optimized)
release:
    cargo build --release --features cli

# Run all tests
test:
    cargo test

# Run lint checks
clippy:
    cargo clippy --all-targets -- -D warnings

# Format code
fmt:
    cargo fmt

# Check formatting
fmt-check:
    cargo fmt --check

# Review local changes against main (default: current directory)
review path=".":
    review-engine review --local-path {{path}} --base main

# Review staged changes
review-staged path=".":
    review-engine review --local-path {{path}} --staged

# Self-review the project
self-review:
    review-engine review --local-path . --base main --format markdown

# Start health check server
serve port="8080":
    review-engine serve --port {{port}}

# Install release binary to ~/.local/bin
install:
    cp target/release/review-engine {{home()}}/.local/bin/review-engine

# Complete check (fmt + clippy + test)
check: fmt-check clippy test

# Clean build artifacts
clean:
    cargo clean

# Show version
version:
    review-engine --version

# Validate config
validate config=".code-audit-config.toml":
    review-engine validate --config {{config}}

# Generate config schema documentation
schema:
    @echo "Configuration Schema"
    @echo "===================="
    @echo ""
    @echo "See docs/config-schema.md for complete configuration reference."
