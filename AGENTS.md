# AGENTS.md — test-r

## Build & Test
- Build: `cargo build --all-features`
- Test all: `cargo test -p test-r --all-features`
- Single test: `cargo test -p test-r --all-features <test_name>`
- Lint: `cargo clippy --no-deps --all-targets -- -Dwarnings`
- Format check: `cargo fmt --all -- --check`
- License check: `cargo deny check`

## Architecture
Rust workspace with three core crates and two examples:
- **test-r** — Public API crate; re-exports macros and core types. Entry point for users.
- **test-r-core** — Runtime: test runner, CLI args, output formats, IPC, benchmarking, execution.
- **test-r-macro** — Proc macros (`#[test]`, `#[bench]`, `test_dep`, `test_gen`, `define_matrix_dimension`, etc.). Uses `syn`/`quote`/`darling`.
- **example** / **example-tokio** — Example/integration test crates (compile-only in CI).
- **github** — CI workflow generation via `gh-workflows` (build.rs).

## CI (github/)
- `.github/workflows/` YAML files are **auto-generated** — do NOT edit them by hand.
- CI is defined in `github/src/main.rs` using the `gh-workflow` crate. Edit that file, then run `cargo run -p test-r-github` to regenerate workflows.

## Documentation (book/)
- The `book/` directory is an **mdBook**. Update it when adding or changing user-facing features.
- Build/verify: `mdbook build book` (install with `cargo install mdbook` if needed).
- Source lives in `book/src/`; `SUMMARY.md` is the table of contents.

## Sync/Async Design
- test-r must be feature-complete **without** tokio (sync mode). The `tokio` feature flag adds async test support.
- Every new feature should work in both sync and async modes when possible, with minimal code duplication.

## Code Style
- Rust 2024 edition (workspace), some crates still 2021. Resolver v3.
- Use `cargo fmt` (rustfmt) formatting. Run `cargo clippy` with `-Dwarnings`.
- Prefer `parking_lot` over `std::sync`. Use `anyhow` for fallible results where enabled.
- Proc macros use `darling` for attribute parsing, `syn`/`quote` for codegen.
- Feature flags: `tokio` (async runtime support) and `anyhow` (error handling), both default-on.
