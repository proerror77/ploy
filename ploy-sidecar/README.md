# ploy-sidecar

Research, oversight, diagnostics, and proposal-only safety sidecar powered by the Claude Agent SDK.

It orchestrates a read-only operator loop every few minutes while reading the trading-platform control plane exposed by `ployd`.

## Architecture

```
Claude Sidecar (Sonnet/Opus)
├── espn MCP       → Live NBA scores, quarter, clock
├── polymarket MCP → Market search and snapshots
├── WebSearch      → X.com sentiment, injuries, momentum
├── diagnostics MCP  → `ployctl system diagnose`, `ployctl trading diagnose`, proposal creation
├── research MCP     → `ployctl research replay/backtest/compare/oversight`
└── ploy-backend MCP
    ├── get_system_status    → ployd health + uptime
    ├── get_trading_state    → canonical trading snapshots
    ├── list/get_deployment  → deployment resources
    └── read-only control-plane inspection
```

The sidecar does research, diagnostics, proposal creation, and operator inspection. The platform control plane owns deployment lifecycle and trading state, and operators still approve every safety proposal before it can change runtime behavior.

## Installation

```sh
cd ploy-sidecar
npm install
```

Requires Node.js 22+.

## Configuration

Copy `.env.example` to `.env`:

```sh
cp .env.example .env
```

For MiniMax, you can start from:

```sh
cp .env.minimax.example .env
```

| Variable | Default | Description |
|---|---|---|
| `ANTHROPIC_API_KEY` | — | Anthropic-compatible API key (required) |
| `ANTHROPIC_BASE_URL` | — | Optional Anthropic-compatible base URL (MiniMax examples: `https://api.minimaxi.com/anthropic` or `https://api.minimax.io/anthropic`) |
| `ANTHROPIC_CUSTOM_HEADERS` | — | Optional custom headers, one per line in `Header: Value` format (example: `Authorization: Bearer <key>`) |
| `MINIMAX_ANTHROPIC_MODEL` | `MiniMax-M2.5` | Optional MiniMax model id used for automatic alias mapping when `ANTHROPIC_BASE_URL` points to MiniMax |
| `PLOY_API_URL` | `http://localhost:8081` | `ployd` control-plane URL |
| `PLOY_API_KEY` | — | Bearer token (optional) |
| `PLOY_API_ADMIN_TOKEN` | — | Admin token for deployment apply/control requests |
| `PLOY_SIDECAR_AUTH_TOKEN` | — | Optional sidecar token header for control-plane requests |
| `SIDECAR_MODEL` | `sonnet` | Model name or alias (`sonnet`, `opus`, `haiku`, or a full model id like `claude-opus-4-6` / `MiniMax-M2.5`) |
| `SIDECAR_POLL_INTERVAL_SECS` | `300` | Scan interval (seconds) |
| `SIDECAR_MAX_BUDGET_USD` | `1.00` | Max Claude cost per scan cycle |
| `SIDECAR_DRY_RUN` | `true` | Keep the sidecar in recommendation-only mode |

## MiniMax M2.5 (Anthropic-Compatible)

If you want to use **MiniMax M2.5** via their **Anthropic-compatible** endpoint (instead of Claude models), set:

```sh
export ANTHROPIC_BASE_URL="https://api.minimaxi.com/anthropic"
export ANTHROPIC_API_KEY="YOUR_MINIMAX_API_KEY"
export SIDECAR_MODEL="MiniMax-M2.5"
# Optional for Anthropic-compatible providers that require explicit Authorization:
export ANTHROPIC_CUSTOM_HEADERS=$'Authorization: Bearer YOUR_MINIMAX_API_KEY'
```

When `ANTHROPIC_BASE_URL` points to MiniMax, the sidecar now auto-applies:

- `Authorization: Bearer ...` header (if `ANTHROPIC_CUSTOM_HEADERS` is unset)
- alias mapping for `opus` / `sonnet` / `haiku` to `MINIMAX_ANTHROPIC_MODEL` (default `MiniMax-M2.5`)

If you get `invalid api key` on one MiniMax domain, switch to the other domain above (accounts are often region-bound).

If you prefer to keep using model aliases like `opus` in configs, you can also map Claude aliases to MiniMax by setting:

```sh
export ANTHROPIC_DEFAULT_OPUS_MODEL="MiniMax-M2.5"
export ANTHROPIC_DEFAULT_SONNET_MODEL="MiniMax-M2.5"   # optional
export ANTHROPIC_DEFAULT_HAIKU_MODEL="MiniMax-M2.5"    # optional
```

## Usage

Start the platform daemon first:

```sh
# Terminal 1 — trading-platform daemon
cargo run -p ployd
cargo run -p ployctl -- system status
```

Then start the sidecar:

```sh
# Terminal 2 — TypeScript sidecar
npm run dev
```

### Development mode (dry-run, verbose output)

```sh
SIDECAR_DRY_RUN=true SIDECAR_POLL_INTERVAL_SECS=60 npm run dev
```

### Production

```sh
npm run build
node dist/index.js
```

## Decision Pipeline

Each scan cycle:

1. **Platform inspection** — fetch `/api/system/status`, `/api/deployments`, and `/api/trading/state`
2. **Rust oversight check** — run `ployctl research oversight` to get deterministic signals and playbook actions
3. **Diagnostics pass** — run `ployctl system diagnose` or `ployctl trading diagnose <deployment-id>` when a deployment looks suspicious
4. **Research pass** — run replay, backtest, or config compare only when they help explain current state
5. **Proposal pass** — create a safety proposal only when the evidence supports operator review; proposals never execute runtime mutations directly
6. **External context** — use ESPN, Polymarket, and WebSearch only as supporting evidence
7. **Recommendation** — emit alerts and operator recommendations only; no live mutation path exists in the sidecar

## Control-Plane Endpoints

The sidecar aligns to the trading-platform operator surface:

```
GET  /api/system/status   Platform health snapshot
GET  /api/trading/state   Canonical trading-state snapshots
GET  /api/deployments     Deployment summaries
GET  /api/deployments/:id Deployment resource details
POST /api/proposals       Create operator-approved safety proposal
```

The sidecar does not call deployment mutation or intent-ingress endpoints. Proposal creation is the maximum authority it gets, and every proposal still requires an operator to approve it through `ployctl` or the frontend. Legacy `/api/sidecar/*`, `/api/config`, and direct `enable/disable` mutations are not part of the default workspace control plane on this branch.

## Output

Each scan produces structured JSON:

```json
{
  "summary": {
    "timestamp": "2026-04-07T10:00:00Z",
    "platform_status": "degraded",
    "deployments_reviewed": 3,
    "research_tasks": 1,
    "oversight_alerts": 2,
    "operator_recommendations": 2
  },
  "research_reports": [
    {
      "subject": "example.paper",
      "kind": "diagnostic",
      "status": "completed",
      "finding": "state mismatch preceded pnl regression",
      "evidence": ["desired_state=running", "observed_state=degraded", "net_pnl=-2.50"]
    }
  ],
  "oversight_alerts": [
    {
      "severity": "critical",
      "deployment_id": "example.paper",
      "kind": "pnl_regression",
      "message": "net pnl deteriorated to -2.50",
      "recommended_action": "backtest"
    }
  ],
  "operator_recommendations": [
    {
      "kind": "diagnose",
      "target": "example.paper",
      "rationale": "state mismatch and pnl regression need a root-cause report",
      "evidence": ["ployctl trading diagnose example.paper"]
    },
    {
      "kind": "create_proposal",
      "target": "example.paper",
      "rationale": "operator should review whether this deployment needs a pause",
      "evidence": ["proposal_id=proposal-example-paper-123"]
    }
  ]
}
```

Each cycle also appends a trace record to `run/sidecar/agent-runs.jsonl` with run id, tool calls, cost, failure reason, and lightweight evaluation metadata.

## File Structure

```
ploy-sidecar/
├── src/
│   ├── index.ts              Main loop (Claude Commander)
│   ├── runtime/
│   │   ├── diagnostics.ts    Diagnostics type helpers
│   │   └── run-recorder.ts   JSONL trace persistence
│   ├── tools/
│   │   ├── diagnostics.ts    Diagnostics MCP server
│   │   ├── espn.ts           ESPN MCP server
│   │   ├── polymarket.ts     Polymarket MCP server
│   │   ├── ploy-backend.ts   Ploy Rust backend MCP server
│   │   └── research.ts       Ploy CLI research MCP server
│   ├── schemas/
│   │   └── output.ts         Structured output JSON schema
│   └── hooks/
│       └── risk-guard.ts     Optional paper-mode deployment guard
├── .env.example
└── package.json
```
