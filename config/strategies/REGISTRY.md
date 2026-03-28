# Strategy Registry

Authoritative index of all trading strategies. Each strategy has a unique
numeric ID used as filename prefix. Config naming convention:

```
{NN}-{strategy-name}.{mode}.toml
```

**Modes**: `default` (baseline), `dryrun` (prod-matched dry), `live` (real
money), `canary` (single-symbol live test), `backtest` (relaxed for replay),
`template` (disabled reference)

---

## Active Strategies (with config files)

| ID | Strategy | Domain | Description |
|----|----------|--------|-------------|
| 01 | momentum | Crypto | CEX lag momentum — buys PM side predicted to win when Binance moves |
| 02 | pm5-directional | Crypto | Log-normal P(S_T>=S_0) on 5-min binary options via Binance oracle |
| 03 | pattern-memory | Crypto | Bayesian beta-posterior on kline return pattern similarity |
| 04 | staggered-arb | Crypto | Time-separated two-leg arb (Leg1 + hedge Leg2, sum < $1) |
| 05 | split-arb | Crypto | Simultaneous YES+NO when combined < $1 |
| 06 | gamma-scalping | Crypto | Delta-neutral straddle, rebalance on vol |
| 07 | liquidity-vacuum | Crypto | Mean-reversion against panic flow (disabled template) |

## Code-Only Strategies (no config files yet)

| ID | Strategy | Domain | Description |
|----|----------|--------|-------------|
| 08 | volatility-arb | Crypto | IV vs realized vol edge (Black-Scholes) |
| 09 | deribit-prob-arb | Cross-exch | Deribit-implied P(UP) vs Polymarket |
| 10 | crypto-lob-ml | Crypto | Order book ML feature inference |
| 11 | crypto-rl-policy | Crypto | RL policy network (requires `rl` feature) |
| 12 | dump-hedge | Crypto | Panic dump detection + progressive hedge |
| 13 | nba-comeback | Sports | NBA Q3-Q4 comeback win probability |
| 14 | event-edge | Events | Arena/data-source mispricing scanner |

---

## Config Files

### S01 — Momentum
| File | Mode | Notes |
|------|------|-------|
| `01-momentum.default.toml` | default | 4 symbols, 3 shares, predictive |
| `01-momentum.live-aws.toml` | live | 3 symbols, 4 shares, confirmatory |

### S02 — PM 5-Min Directional
| File | Mode | Notes |
|------|------|-------|
| `02-pm5d.dryrun.toml` | dryrun | 3 symbols, backtest-matched gates (cooldown 60s, min_time 60s) |
| `02-pm5d.live.toml` | live | 3 symbols, $1/trade, hold-to-resolution |
| `02-pm5d.canary.toml` | canary | BTC only, same gates as dryrun |
| `02-pm5d.backtest-relaxed.toml` | backtest | Relaxed OBI (3%), loose timing |

### S03 — Pattern Memory
| File | Mode | Notes |
|------|------|-------|
| `03-pattern-memory.default.toml` | default | 4 symbols (BTC/ETH/SOL/XRP), corr 0.70 |

### S04 — Staggered Arb
| File | Mode | Notes |
|------|------|-------|
| `04-staggered-arb.live.toml` | live | 3 symbols, 20 shares, sum<0.92 |

### S05 — Split Arb
| File | Mode | Notes |
|------|------|-------|
| `05-split-arb.default.toml` | default | 50 shares, target sum 98c |

### S06 — Gamma Scalping
| File | Mode | Notes |
|------|------|-------|
| `06-gamma-scalping.default.toml` | default | 3 symbols, $1/leg |

### S07 — Liquidity Vacuum
| File | Mode | Notes |
|------|------|-------|
| `07-liquidity-vacuum.template.toml` | template | Disabled, reference only |
