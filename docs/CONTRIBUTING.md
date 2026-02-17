# Contributing to Ploy

Guidelines for contributing to the Ploy Polymarket trading bot.

## Development Setup

### Prerequisites

- **Rust** (stable toolchain) -- install via [rustup](https://rustup.rs/)
- **PostgreSQL 15+** -- used for event registry, position tracking, and audit logs
- **pkg-config**, **libssl-dev**, **libpq-dev** (Linux) or equivalent (macOS: `brew install postgresql openssl`)
- **Node.js 18+** (only if working on the NBA Swing frontend)

### Clone and Build

```bash
git clone <repo-url> && cd ploy

# Default build (no optional features)
cargo build

# With reinforcement learning support
cargo build --features rl

# With ONNX inference
cargo build --features onnx

# Full feature set
cargo build --features "rl,onnx,analysis"
```

### Environment

Create a `.env` file (never committed) with at minimum:

```
DATABASE_URL=postgres://ploy:ploy@localhost:5432/ploy
```

Additional variables depend on which domain agents you run (Polymarket API keys, Grok API key, etc.). See the project configuration files for the full list.

## Git Workflow

### Branch Naming

| Prefix      | Purpose                        |
|-------------|--------------------------------|
| `feat/`     | New features                   |
| `fix/`      | Bug fixes                      |
| `refactor/` | Code restructuring (no behavior change) |
| `docs/`     | Documentation only             |
| `test/`     | Adding or updating tests       |
| `chore/`    | Build, CI, dependency updates  |

Example: `feat/kelly-scaling-in`, `fix/circuit-breaker-reset`

### Commit Messages

Follow the **atomic commit** convention -- one commit per logical change.

Format:

```
type(scope): short description
```

**Types**: `feat`, `fix`, `refactor`, `docs`, `test`, `chore`

**Rules**:
- Keep refactors, formatting, and behavior changes in **separate** commits.
- Each commit must build (`cargo build`) and pass relevant tests.
- Avoid WIP commits on shared branches.

Good examples:

```
feat(agents): add ML/ONNX model loading for crypto + RL
fix(circuit_breaker): reset half_open_successes on state transition
refactor(engine): extract slippage protection into separate module
docs: update agent framework design with Phase 1.5 Kelly scaling-in
```

### Pull Request Process

1. Create a feature branch from `main`.
2. Make atomic commits following the conventions above.
3. Push and open a PR with a clear description of the changes.
4. Ensure CI passes (formatting, clippy, build, tests).
5. Request review if the change touches risk management, order execution, or security-sensitive code.

## CI/CD

### Test Pipeline

Every push to `main` and every pull request triggers the **Test** workflow (`.github/workflows/test.yml`):

1. **Formatting check** -- `cargo fmt --all -- --check`
2. **Clippy lints** -- `cargo clippy --all-targets --features rl -- -D warnings`
3. **Build** -- `cargo build --features rl`
4. **Tests** -- `cargo test --features rl` (against a PostgreSQL 15 service container)

A separate **Build Check** job verifies the release profile compiles (`cargo build --release --features rl`).

### Deployment Pipelines

Deployment workflows (in `.github/workflows/deploy-*.yml`) handle building, packaging, and deploying to AWS EC2 via S3 + SSM. These are triggered on pushes to `main` or via manual dispatch. Deployment requires `AWS_ACCESS_KEY_ID` and `AWS_SECRET_ACCESS_KEY` repository secrets.

## Running Tests

```bash
# Run all tests (default features)
cargo test

# Run tests with RL feature enabled (matches CI)
cargo test --features rl

# Run a specific test
cargo test test_name

# Run tests for a specific module
cargo test --lib strategy::
```

A running PostgreSQL instance is required for integration tests. Set `DATABASE_URL` in your environment or `.env` file.

## Code Style

- Run `cargo fmt` before committing.
- Run `cargo clippy --all-targets` and address warnings.
- Use `thiserror` for library-style errors, `anyhow` sparingly at application boundaries.
- Prefer `rust_decimal::Decimal` over floating-point for monetary values.
- Use `zeroize` for any secret material (private keys, API keys).
- Keep `unsafe` blocks to zero; the codebase currently has none.

## Feature Flags

| Flag       | Purpose                              | Crate(s)                    |
|------------|--------------------------------------|-----------------------------|
| `rl`       | Reinforcement learning (PPO)         | burn, burn-ndarray, bincode |
| `onnx`     | ONNX model inference                 | tract-onnx                  |
| `analysis` | Offline Parquet analysis via DuckDB  | duckdb                      |
| `api`      | API module with SQLx compile checks  | (requires DATABASE_URL)     |

## Project Structure

```
src/
  agents/       -- Domain trading agents (crypto, sports, politics)
  coordinator/  -- Multi-agent coordinator and bootstrap
  strategy/     -- Strategy logic (arb, momentum, registry)
  services/     -- External service integrations (CLOB, discovery)
  risk/         -- Risk management (circuit breaker, position limits)
  tui/          -- Terminal UI (ratatui)
  api/          -- HTTP/WebSocket API server (axum)
```
