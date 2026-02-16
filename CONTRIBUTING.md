# Contributing to rllvm

Thanks for your interest in contributing! This guide covers the essentials.

## Development Setup

1. **Install Rust** (stable, edition 2024, MSRV 1.85):
   ```bash
   rustup update stable
   ```

2. **Install LLVM/Clang** (required for building and testing):
   - macOS: `brew install llvm`
   - Linux: `sudo apt install llvm llvm-dev clang libclang-dev`

3. **Build**:
   ```bash
   cargo build --verbose
   ```

## Code Style

CI enforces both `rustfmt` and `clippy`. Please run these before submitting:

```bash
cargo fmt --all                              # Auto-format
cargo fmt --all --check                      # Check formatting (what CI runs)
cargo clippy --all-targets -- -D warnings    # Lint
```

- Follow standard Rust naming conventions.
- Keep changes focused — avoid unrelated reformatting or refactoring in the same PR.

## Running Tests

```bash
cargo test --all --verbose
```

LLVM/Clang must be installed for the test suite to pass. Tests run on both Linux and macOS in CI.

## Pull Request Process

1. **Fork and branch** — create a feature branch from `main`.
2. **Make your changes** — keep commits small and well-described.
3. **Ensure CI passes** — format, clippy, tests, and `cargo audit` all run automatically.
4. **Open a PR against `main`** — describe what you changed and why.
5. **Respond to review feedback** — maintainers may request changes before merging.

## License

By contributing, you agree that your contributions will be licensed under the [GPL-3.0 license](LICENSE).
