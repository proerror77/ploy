# Review Scope

## Target

Full Ploy trading system codebase — a Polymarket trading bot written in Rust (~165K lines, 260+ source files) with TypeScript sidecar and React frontend.

## Components

### Rust Core (src/) — ~260 files, ~165K lines
- **adapters/**: Exchange connectors (Polymarket CLOB/WS, Binance, Kalshi, Chainlink), PostgreSQL, Feishu, API server
- **agents/**: Domain trading agents (crypto, sports, politics) + OpenClaw integration
- **ai_clients/**: Grok/LLM integration, autonomous trading, sports analysis
- **analysis/**: Pattern memory backtest, up/down backtest
- **api/**: Axum REST API + WebSocket server, auth, handlers, routes
- **cli/**: CLI commands, Polymarket subcommands (pm/*), RPC, service management
- **collector/**: Market data collection (Binance depth/klines, Polymarket orderbook)
- **config.rs**: Configuration system
- **coordination/**: Circuit breaker, emergency stop, lifecycle, shutdown
- **coordinator/**: Multi-agent coordinator, bootstrap, state management
- **domain/**: Core domain models (market, order, state)
- **exchange/**: Exchange abstraction layer (factory, traits)
- **main*.rs**: Entry points, dispatch, runtime, agent modes
- **ml/**: Dense neural network, ONNX inference
- **persistence/**: Checkpoint, DLQ processor, event store
- **platform/**: Platform orchestration, risk, queue, position, data plane, subscriptions
- **rl/**: Reinforcement learning (PPO, environments, training, networks)
- **safety/**: Direct live trading safety checks
- **services/**: Health, metrics, discovery, order monitor, data collector
- **signing/**: Wallet signing, HMAC, nonce management, order signing
- **strategy/**: Core trading strategies — split arb, staggered arb, momentum, NBA comeback, event edge, gamma scalping, pattern memory, directional, volatility arb, liquidity vacuum
- **supervisor/**: Watchdog, alert manager, playbook
- **tui/**: Terminal UI (ratatui) — app, widgets, events, theme
- **validation.rs**: Input validation

### TypeScript Sidecar (ploy-sidecar/) — 6 files
- Claude Agent SDK integration for NBA comeback research
- MCP tools: ESPN, Polymarket, Ploy backend
- Risk guard hook

### React Frontend (ploy-frontend/) — 22 files
- Dashboard, live monitor, strategy config, risk dashboard
- WebSocket + REST API services
- Zustand state management

### Infrastructure
- **migrations/**: 22 SQL migration files (PostgreSQL schema)
- **config/**: TOML strategy configurations (10 files)
- **.github/workflows/**: 11 CI/CD workflows (deploy, release, test, rollback)
- **deployment/**: Production TOML configs

## Files

### Rust Source (260+ files)
All files under `src/` — see component breakdown above.

### Frontend Source (22 files)
All files under `ploy-frontend/src/`

### Sidecar Source (6 files)
All files under `ploy-sidecar/src/`

### Database Migrations (22 files)
All files under `migrations/`

### CI/CD Workflows (11 files)
All files under `.github/workflows/`

### Configuration (10+ files)
All files under `config/` and `deployment/`

## Flags

- Security Focus: no
- Performance Critical: no
- Strict Mode: no
- Framework: Rust/Tokio/Axum (auto-detected)

## Review Phases

1. Code Quality & Architecture
2. Security & Performance
3. Testing & Documentation
4. Best Practices & Standards
5. Consolidated Report
