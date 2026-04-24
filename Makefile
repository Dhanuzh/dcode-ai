.PHONY: all build dev release clean test install fmt lint help

# Default target
all: build

# Build debug version
build:
	cargo build

# Build release version (optimized)
release:
	cargo build --release

# Build for a specific package
build-%:
	cargo build --package $* --release

# Install binaries to /usr/local/bin
install: release
	@echo "Installing dcode-ai to /usr/local/bin..."
	cp target/release/dcode-ai /usr/local/bin/dcode-ai
	@echo "Installed successfully!"

# Run development build
dev:
	cargo build
	@echo "Dev binaries built at target/debug/"

# Run tests
test:
	cargo test

# Run tests for a specific package
test-%:
	cargo test --package $*

# Format code
fmt:
	cargo fmt

# Lint code
lint:
	cargo clippy -- -D warnings

# Clean build artifacts
clean:
	cargo clean

# Build and show binary sizes
sizes: release
	@echo "=== Binary Sizes ==="
	@ls -lh target/release/dcode-ai 2>/dev/null || echo "Binary not found"

# Run with custom config
run-dev: dev
	./target/debug/dcode-ai

# Generate shell completions
completions:
	./target/release/dcode-ai completion bash > contrib/dcode-ai.bash
	./target/release/dcode-ai completion zsh > contrib/_dcode-ai

# Run benchmarks (if any)
bench:
	cargo bench

# Check formatting
check-fmt:
	cargo fmt -- --check

# Update dependencies
update:
	cargo update

# Help
help:
	@echo "Available targets:"
	@echo "  build     - Build debug version"
	@echo "  release   - Build optimized release version"
	@echo "  install   - Install binaries to /usr/local/bin"
	@echo "  dev       - Build development version"
	@echo "  test      - Run tests"
	@echo "  test-*    - Run tests for specific package"
	@echo "  fmt       - Format code"
	@echo "  lint      - Lint code"
	@echo "  clean     - Clean build artifacts"
	@echo "  sizes     - Show binary sizes"
	@echo "  completions - Generate shell completions"
	@echo "  bench     - Run benchmarks"
	@echo "  update    - Update dependencies"
	@echo "  check-fmt - Check code formatting"