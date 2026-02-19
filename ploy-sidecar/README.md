# ploy-sidecar

NBA comeback trading research agent powered by Claude Agent SDK.

Orchestrates a multi-tool research pipeline every 5 minutes and routes final trade decisions through the Ploy Rust backend.

## Architecture

```
Claude Commander (Sonnet/Opus)
├── espn MCP       → Live game scores, quarter, clock
├── polymarket MCP → Market search, order book snapshot
├── WebSearch      → X.com sentiment, injuries, momentum
└── ploy-backend MCP
    ├── request_grok_decision  → Grok final judge (via Rust)
    └── submit_order           → Order execution (via Rust Coordinator)
```

The sidecar does research. The Rust backend executes. Grok is the final judge.

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
| `PLOY_API_URL` | `http://localhost:8081` | Ploy Rust backend URL |
| `PLOY_API_KEY` | — | Bearer token (optional) |
| `SIDECAR_MODEL` | `sonnet` | Model name or alias (`sonnet`, `opus`, `haiku`, or a full model id like `claude-opus-4-6` / `MiniMax-M2.5`) |
| `SIDECAR_POLL_INTERVAL_SECS` | `300` | Scan interval (seconds) |
| `SIDECAR_MAX_BUDGET_USD` | `1.00` | Max Claude cost per scan cycle |
| `SIDECAR_DRY_RUN` | `true` | Set to `false` for live orders |

Grok is configured on the **Rust backend** side via `GROK_API_KEY`.

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

Start the Rust backend first (required for Grok decisions and order execution):

```sh
# Terminal 1 — Rust backend with platform mode
GROK_API_KEY=... ploy platform --sports
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

1. **ESPN scan** — fetch today's live NBA games
2. **Filter** — Q3 or early Q4 games with 1–15 point deficit
3. **Market lookup** — find corresponding Polymarket market
4. **Risk check** — reward-to-risk ≥ 4x (price ≤ $0.20), EV ≥ 5%
5. **X.com research** — injuries, momentum, betting sentiment via WebSearch
6. **Grok decision** — submit research brief to Grok (via Rust backend); only trade if Grok says "trade"
7. **Order submission** — through Rust Coordinator (RiskGate → Queue → Execution)

## Risk Controls

Two layers:

**TypeScript hook** (enforced before every `submit_order`):
- Max order size: $50
- Max entry price: $0.20 (4× reward-to-risk threshold)
- Forces `dry_run=true` when `SIDECAR_DRY_RUN=true`

**Rust Coordinator** (enforced server-side):
- Daily loss limit
- Max single exposure
- Position sizing via RiskGate

## Sidecar API Endpoints

The Rust backend exposes these endpoints for the sidecar:

```
POST /api/sidecar/grok/decision   Grok unified trade decision
POST /api/sidecar/orders          Submit order through Coordinator
GET  /api/sidecar/positions       Current open positions
GET  /api/sidecar/risk            Coordinator risk state
```

## Output

Each scan produces structured JSON:

```json
{
  "scan_summary": { "games_scanned": 4, "in_progress_games": 2, "comeback_candidates": 1 },
  "opportunities": [
    {
      "trailing_team": "LAL",
      "deficit": 8,
      "quarter": 3,
      "market_price": 0.18,
      "reward_risk_ratio": 4.56,
      "expected_value": 0.07,
      "action": "TRADE",
      "grok_decision": "trade",
      "confidence": "high",
      "reasoning": "..."
    }
  ],
  "orders_submitted": [
    { "market_slug": "lakers-vs-nuggets-2026-02-17", "shares": 50, "price": 0.18, "dry_run": true, "status": "submitted" }
  ]
}
```

## File Structure

```
ploy-sidecar/
├── src/
│   ├── index.ts              Main loop (Claude Commander)
│   ├── tools/
│   │   ├── espn.ts           ESPN MCP server
│   │   ├── polymarket.ts     Polymarket MCP server
│   │   └── ploy-backend.ts   Ploy Rust backend MCP server
│   ├── schemas/
│   │   └── output.ts         Structured output JSON schema
│   └── hooks/
│       └── risk-guard.ts     PreToolUse risk enforcement hook
├── .env.example
└── package.json
```
