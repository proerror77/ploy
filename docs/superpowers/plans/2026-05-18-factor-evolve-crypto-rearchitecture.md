# FactorEvolve Crypto Re-Architecture Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Turn the current PM5D/AutoFactor research pipeline into a first-class LOB Factor Research OS with constrained LLM priors, deterministic Rust evaluation, durable factor lineage, dry-run feedback, and human-gated promotion.

**Architecture:** Keep the existing Ploy runtime boundary: production trading stays under `ployd` / `ployctl` / strategy configs, while research lives in `crates/ploy-research` and CI artifacts. The refactor adds durable registry/trace tables, a real Binance futures local orderbook lane, a typed Research Manager orchestration surface, and portfolio/dry-run feedback gates without letting LLM output bypass Rust validators or promotion gates.

**Tech Stack:** Rust workspace, PostgreSQL migrations, Parquet/ZSTD research snapshots, GitHub Actions, Python compatibility scripts where already established, `rtk` command wrappers, `gh` workflow control, `tango-1-1` remote verification.

---

## Remote Baseline Checked On 2026-05-18

- GitHub Actions:
  - `Test` on `main` succeeded at run `26037715583`.
  - `Factor Walk-Forward V2 Hosted Artifact` succeeded at run `26036588757`.
  - `Runtime Candidate Replay` succeeded at run `26036775617`.
  - Two newer `Runtime Candidate Replay` runs, `26037732928` and `26037733115`, were still in progress during planning.
  - Scheduled `Healthcheck tango-1-1` run `26034356796` was pending, and scheduled `Market Data Gap Audit` run `26033640182` was waiting.
- `tango-1-1` runtime:
  - `ployd.service` active; `Restart=always`, `OOMPolicy=kill`, `MemoryMax=1610612736`.
  - `/health` returned `database_connected=true`, `active_alert_count=0`, `stale_source_count=0`, `error_count_1h=0`.
  - Active services checked as active: `ployd`, Binance aggTrade, Binance price, Binance LOB, quote collector, PM trade collector, orderbook archive timer, orderbook retention timer.
- Data freshness:
  - Recent 15 minute counts were healthy for `binance_lob_ticks`, `binance_agg_trade_ticks`, `clob_quote_ticks`, and `clob_orderbook_snapshots`.
  - `clob_trade_ticks` recent freshness query hit a 5s timeout; create an indexed/narrow health check instead of using broad `max()` scans.
- Dry-run state:
  - `/api/deployments` shows `pm5d.threelayer.settlement-probability-btc-eth.dryrun` as desired/observed `running`.
  - `/api/reports/dry-run` generated at `2026-05-18T13:56:55Z` but its visible strategy summaries were old May 2-3 samples from four legacy PM5D dry-run variants, all losing overall. The currently running settlement-probability deployment was not clearly represented as "running with no recent closed trades"; this is a reporting gap.

## Current Fit Against PRD

### Already aligned

- `docs/PROJECT_SEMANTICS.md` defines research stages and blocks promotion when full-depth CLOB, official settlement, replay parity, or runtime scorer parity is missing.
- `crates/ploy-research/src/autofactor.rs` has a constrained `FactorExpr` AST and safe evaluator rather than free-form code execution.
- `crates/ploy-research/src/alpha_search.rs` emits search-space, candidate, rejected, node-metric, MCTS state/plan, avoided-subtree, and feedback artifacts.
- Hosted factor walk-forward can chain alpha-search iterations and feed ready handoffs into guarded dry-run config PRs.
- AutoFactor promotion is fail-closed on runtime mapping, event-level one-decision evidence, full-depth fillability, symbol stability, and global PRD gates.

### Main gaps

- Binance LOB is currently partial depth snapshot collection into `binance_lob_ticks`, not a futures diff-depth local book builder with snapshot + incremental update sequence validation.
- Durable `factor_registry`, `factor_evaluations`, and append-only `experiment_trace` tables now have a first-class migration, but the writer/query service is still incomplete.
- Research Manager is artifact/classifier driven, not a typed LLM prior generation service that reads trace and writes bounded prior proposals.
- Portfolio Builder is not yet a first-class module with de-correlation, marginal Sharpe, turnover/capacity penalty, and promotion output.
- Dry-run feedback does not cleanly expose the currently running candidate as a first-class report row when it has no recent closed trades.
- Multi-exchange and external data surfaces from the PRD, especially OKX/Bybit LOB, Binance futures OI/funding/liquidation/basis, are not first-class.

## File Structure

- Create: `migrations/042_factor_research_os_registry.sql`
  - Owns durable `factor_registry`, `factor_evaluations`, `experiment_trace`, and status constraints.
- Create: `crates/ploy-research/src/research_os/mod.rs`
  - Public module boundary for FactorEvolve research contracts.
- Create: `crates/ploy-research/src/research_os/registry.rs`
  - Rust structs for factor registry/evaluation/trace payloads and serialization tests.
- Create: `crates/ploy-research/src/research_os/trace.rs`
  - Append-only hash-chain helpers and tests.
- Create: `crates/ploy-research/src/orderbook/mod.rs`
  - Local orderbook module boundary.
- Create: `crates/ploy-research/src/orderbook/binance_futures.rs`
  - Binance futures snapshot + diff-depth sequencing model and replay validator.
- Create: `crates/ploy-research/src/research_os/manager.rs`
  - Typed Research Manager input/output contracts, no live LLM call in the first slice.
- Create: `crates/ploy-research/examples/factor_evolve_daily_plan.rs`
  - Dry-run planner that reads prior artifacts/registry summaries and emits a next research plan JSON.
- Modify: `crates/ploy-research/src/lib.rs`
  - Export `research_os` and `orderbook` modules.
- Modify: `crates/ploy-research/src/autofactor.rs`
  - Add stable factor expression hash helper and registry conversion only after tests.
- Modify: `crates/ploy-research/src/alpha_search.rs`
  - Write registry-compatible candidate and trace summaries into artifact bundle.
- Modify: `.github/workflows/factor-walk-forward-v2-hosted-artifact.yml`
  - Upload registry/trace artifacts and pass them to the closed-loop classifier.
- Modify: `scripts/alpha_search_closed_loop_agent.py`
  - Add output fields that map directly to Research Manager typed prior input.
- Modify: `scripts/check_dryrun_report_contract.py`
  - Require running deployments with no trades to appear as explicit zero-activity strategy rows.
- Modify: `scripts/report_dryrun_summary.py`
  - Include running deployment rows even when no recent closed trades exist.
- Create: `tests/test_factor_research_os_registry.py`
  - Static migration/schema contract tests.
- Create: `tests/test_dryrun_report_running_candidate.py`
  - Regression test for running no-trade candidate reporting.

## Task 1: Durable Registry And Append-Only Trace

**Files:**
- Create: `migrations/042_factor_research_os_registry.sql`
- Create: `crates/ploy-research/src/research_os/mod.rs`
- Create: `crates/ploy-research/src/research_os/registry.rs`
- Create: `crates/ploy-research/src/research_os/trace.rs`
- Modify: `crates/ploy-research/src/lib.rs`
- Test: `tests/test_factor_research_os_registry.py`

- [ ] **Step 1: Add migration static test first**

Create `tests/test_factor_research_os_registry.py` with tests that read migration `042` and assert these concrete contract fields:

```python
from pathlib import Path
import re
import unittest

ROOT = Path(__file__).resolve().parents[1]
MIGRATION = ROOT / "migrations" / "042_factor_research_os_registry.sql"


class FactorResearchOsRegistryMigrationTest(unittest.TestCase):
    def test_factor_registry_tables_and_statuses_exist(self) -> None:
        sql = MIGRATION.read_text(encoding="utf-8")
        for table in [
            "factor_registry",
            "factor_evaluations",
            "experiment_trace",
        ]:
            self.assertIn(f"CREATE TABLE IF NOT EXISTS {table}", sql)
        self.assertRegex(sql, r"status TEXT NOT NULL CHECK .*draft.*compiled.*evaluated.*candidate.*dry_run.*approved.*production.*deprecated")
        self.assertIn("dsl_hash TEXT NOT NULL", sql)
        self.assertIn("ast_json JSONB NOT NULL", sql)
        self.assertIn("hash_prev TEXT", sql)
        self.assertIn("hash_current TEXT NOT NULL", sql)

    def test_experiment_trace_is_append_only_by_trigger(self) -> None:
        sql = MIGRATION.read_text(encoding="utf-8")
        self.assertIn("prevent_experiment_trace_update", sql)
        self.assertIn("prevent_experiment_trace_delete", sql)
        self.assertIn("RAISE EXCEPTION 'experiment_trace is append-only'", sql)


if __name__ == "__main__":
    unittest.main()
```

- [ ] **Step 2: Run test and confirm it fails**

Run:

```bash
python3 -m unittest tests.test_factor_research_os_registry
```

Expected: fail because `migrations/042_factor_research_os_registry.sql` does not exist.

- [ ] **Step 3: Add the migration**

Create `migrations/042_factor_research_os_registry.sql` with:

```sql
-- Migration 042: Factor Research OS registry and append-only trace.

CREATE TABLE IF NOT EXISTS factor_registry (
    factor_id UUID PRIMARY KEY,
    factor_name TEXT NOT NULL,
    factor_family TEXT NOT NULL,
    status TEXT NOT NULL CHECK (
        status IN (
            'draft',
            'compiled',
            'evaluated',
            'candidate',
            'dry_run',
            'approved',
            'production',
            'deprecated'
        )
    ),
    hypothesis TEXT NOT NULL,
    economic_logic TEXT NOT NULL DEFAULT '',
    dsl_source TEXT NOT NULL,
    dsl_hash TEXT NOT NULL,
    ast_json JSONB NOT NULL,
    target TEXT NOT NULL,
    horizon TEXT NOT NULL,
    created_by_agent TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    approved_by TEXT,
    approved_at TIMESTAMPTZ,
    deprecated_at TIMESTAMPTZ,
    metadata JSONB NOT NULL DEFAULT '{}'::jsonb
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_factor_registry_dsl_hash
    ON factor_registry(dsl_hash);
CREATE INDEX IF NOT EXISTS idx_factor_registry_status_family
    ON factor_registry(status, factor_family, created_at DESC);

CREATE TABLE IF NOT EXISTS factor_evaluations (
    eval_id UUID PRIMARY KEY,
    factor_id UUID NOT NULL REFERENCES factor_registry(factor_id),
    run_id TEXT NOT NULL,
    data_snapshot_id TEXT NOT NULL,
    evaluator_version TEXT NOT NULL,
    train_ic DOUBLE PRECISION,
    valid_ic DOUBLE PRECISION,
    test_ic DOUBLE PRECISION,
    oos_ic DOUBLE PRECISION,
    rank_ic DOUBLE PRECISION,
    icir DOUBLE PRECISION,
    sharpe_gross DOUBLE PRECISION,
    sharpe_net DOUBLE PRECISION,
    max_drawdown DOUBLE PRECISION,
    turnover DOUBLE PRECISION,
    poly_ev DOUBLE PRECISION,
    poly_avg_fill DOUBLE PRECISION,
    poly_slippage DOUBLE PRECISION,
    poly_exit_capacity DOUBLE PRECISION,
    reward_total DOUBLE PRECISION,
    passed_gate BOOLEAN NOT NULL DEFAULT false,
    rejection_reason TEXT,
    metrics_json JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_factor_evaluations_factor_time
    ON factor_evaluations(factor_id, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_factor_evaluations_run
    ON factor_evaluations(run_id);

CREATE TABLE IF NOT EXISTS experiment_trace (
    trace_id UUID PRIMARY KEY,
    run_id TEXT NOT NULL,
    parent_trace_id UUID,
    event_type TEXT NOT NULL,
    agent_name TEXT NOT NULL,
    input_json JSONB NOT NULL DEFAULT '{}'::jsonb,
    output_json JSONB NOT NULL DEFAULT '{}'::jsonb,
    hash_prev TEXT,
    hash_current TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_experiment_trace_run_time
    ON experiment_trace(run_id, created_at);
CREATE INDEX IF NOT EXISTS idx_experiment_trace_parent
    ON experiment_trace(parent_trace_id)
    WHERE parent_trace_id IS NOT NULL;

CREATE OR REPLACE FUNCTION prevent_experiment_trace_update()
RETURNS trigger AS $$
BEGIN
    RAISE EXCEPTION 'experiment_trace is append-only';
END;
$$ LANGUAGE plpgsql;

CREATE OR REPLACE FUNCTION prevent_experiment_trace_delete()
RETURNS trigger AS $$
BEGIN
    RAISE EXCEPTION 'experiment_trace is append-only';
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS trg_experiment_trace_no_update ON experiment_trace;
CREATE TRIGGER trg_experiment_trace_no_update
    BEFORE UPDATE ON experiment_trace
    FOR EACH ROW EXECUTE FUNCTION prevent_experiment_trace_update();

DROP TRIGGER IF EXISTS trg_experiment_trace_no_delete ON experiment_trace;
CREATE TRIGGER trg_experiment_trace_no_delete
    BEFORE DELETE ON experiment_trace
    FOR EACH ROW EXECUTE FUNCTION prevent_experiment_trace_delete();
```

- [ ] **Step 4: Add Rust registry and trace contracts**

Add `crates/ploy-research/src/research_os/mod.rs`:

```rust
pub mod registry;
pub mod trace;
```

Add `crates/ploy-research/src/research_os/registry.rs`:

```rust
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FactorLifecycleStatus {
    Draft,
    Compiled,
    Evaluated,
    Candidate,
    DryRun,
    Approved,
    Production,
    Deprecated,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FactorRegistryEntry {
    pub factor_id: String,
    pub factor_name: String,
    pub factor_family: String,
    pub status: FactorLifecycleStatus,
    pub hypothesis: String,
    pub economic_logic: String,
    pub dsl_source: String,
    pub dsl_hash: String,
    pub ast_json: serde_json::Value,
    pub target: String,
    pub horizon: String,
    pub created_by_agent: String,
    pub created_at: DateTime<Utc>,
    pub metadata: serde_json::Value,
}
```

Add `crates/ploy-research/src/research_os/trace.rs`:

```rust
use sha2::{Digest, Sha256};

pub fn trace_hash(
    hash_prev: Option<&str>,
    run_id: &str,
    event_type: &str,
    agent_name: &str,
    input_json: &serde_json::Value,
    output_json: &serde_json::Value,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(hash_prev.unwrap_or("").as_bytes());
    hasher.update(b"\n");
    hasher.update(run_id.as_bytes());
    hasher.update(b"\n");
    hasher.update(event_type.as_bytes());
    hasher.update(b"\n");
    hasher.update(agent_name.as_bytes());
    hasher.update(b"\n");
    hasher.update(input_json.to_string().as_bytes());
    hasher.update(b"\n");
    hasher.update(output_json.to_string().as_bytes());
    format!("{:x}", hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trace_hash_changes_when_output_changes() {
        let input = serde_json::json!({"a": 1});
        let out_a = serde_json::json!({"candidate": "a"});
        let out_b = serde_json::json!({"candidate": "b"});
        let hash_a = trace_hash(None, "run-1", "generate", "research_manager", &input, &out_a);
        let hash_b = trace_hash(None, "run-1", "generate", "research_manager", &input, &out_b);
        assert_ne!(hash_a, hash_b);
    }
}
```

Modify `crates/ploy-research/src/lib.rs`:

```rust
pub mod research_os;
```

- [ ] **Step 5: Verify**

Run:

```bash
python3 -m unittest tests.test_factor_research_os_registry
CARGO_TARGET_DIR=/tmp/ploy-factor-evolve-registry /opt/homebrew/bin/timeout 300 rtk cargo test --locked -p ploy-research research_os --lib
rtk git diff --check
```

Expected: all pass.

- [ ] **Step 6: Commit**

```bash
git add migrations/042_factor_research_os_registry.sql crates/ploy-research/src/lib.rs crates/ploy-research/src/research_os tests/test_factor_research_os_registry.py
git commit -m "research: add factor registry and experiment trace contracts"
```

## Task 2: Binance Futures Local Orderbook Lane

**Files:**
- Create: `crates/ploy-research/src/orderbook/mod.rs`
- Create: `crates/ploy-research/src/orderbook/binance_futures.rs`
- Modify: `crates/ploy-research/src/lib.rs`
- Create: `docs/runbooks/binance-futures-local-orderbook.md`

- [ ] **Step 1: Add sequence validator tests**

Add tests in `binance_futures.rs` for:

- snapshot `last_update_id`
- first diff must satisfy Binance-compatible `first_update_id <= last_update_id + 1 <= final_update_id`
- later diff must have `previous_final_update_id + 1 == first_update_id`
- out-of-order diff returns a typed error

- [ ] **Step 2: Implement minimal in-memory book**

Use Rust structs:

```rust
#[derive(Debug, Clone, PartialEq)]
pub struct BookLevel {
    pub price: f64,
    pub size: f64,
}

#[derive(Debug, Clone)]
pub struct LocalBook {
    pub symbol: String,
    pub last_update_id: u64,
    pub bids: Vec<BookLevel>,
    pub asks: Vec<BookLevel>,
}
```

Apply diffs by replacing/removing price levels, sorting bids descending and asks ascending, and rejecting sequence gaps.

- [ ] **Step 3: Add runbook**

Create `docs/runbooks/binance-futures-local-orderbook.md` stating:

- current `scripts/binance_lob_collector.py` is partial depth snapshot evidence, not sequence-correct diff-depth local book evidence;
- FactorEvolve promotion-grade LOB research requires snapshot + incremental diff sequencing;
- no runtime strategy should treat unsequenced partial depth as queue-position or passive-fill evidence.

- [ ] **Step 4: Verify**

```bash
CARGO_TARGET_DIR=/tmp/ploy-factor-evolve-book /opt/homebrew/bin/timeout 300 rtk cargo test --locked -p ploy-research orderbook --lib
rtk git diff --check
```

- [ ] **Step 5: Commit**

```bash
git add crates/ploy-research/src/lib.rs crates/ploy-research/src/orderbook docs/runbooks/binance-futures-local-orderbook.md
git commit -m "research: add Binance futures local orderbook contract"
```

## Task 3: Registry-Compatible Alpha Search Artifacts

**Files:**
- Modify: `crates/ploy-research/src/autofactor.rs`
- Modify: `crates/ploy-research/src/alpha_search.rs`
- Modify: `.github/workflows/factor-walk-forward-v2-hosted-artifact.yml`

- [ ] **Step 1: Add factor expression hash tests**

Add a test proving two identical `FactorExpr` values produce the same hash and a changed constant or input changes the hash.

- [ ] **Step 2: Implement hash helper**

Add:

```rust
pub fn factor_expr_hash(expr: &FactorExpr) -> Result<String, serde_json::Error> {
    use sha2::{Digest, Sha256};
    let raw = serde_json::to_vec(expr)?;
    let mut hasher = Sha256::new();
    hasher.update(raw);
    Ok(format!("{:x}", hasher.finalize()))
}
```

- [ ] **Step 3: Emit registry preview artifact**

In `alpha_search.rs`, write `factor-registry-preview.json` containing:

- `factor_name`
- `target`
- `dsl_hash`
- `ast_json`
- `status` as `candidate`, `watchlist`, or `rejected`
- `metrics`
- `blockers`

- [ ] **Step 4: Wire workflow artifact upload**

Ensure the hosted artifact workflow includes `factor-registry-preview.json` in the uploaded alpha-search bundle.

- [ ] **Step 5: Verify**

```bash
CARGO_TARGET_DIR=/tmp/ploy-factor-evolve-alpha /opt/homebrew/bin/timeout 300 rtk cargo test --locked -p ploy-research autofactor alpha_search --lib
ruby -e 'require "yaml"; YAML.load_file(".github/workflows/factor-walk-forward-v2-hosted-artifact.yml"); puts "ok"'
rtk git diff --check
```

- [ ] **Step 6: Commit**

```bash
git add crates/ploy-research/src/autofactor.rs crates/ploy-research/src/alpha_search.rs .github/workflows/factor-walk-forward-v2-hosted-artifact.yml
git commit -m "research: emit registry-compatible alpha search artifacts"
```

## Task 4: Research Manager Typed Planning Surface

**Files:**
- Create: `crates/ploy-research/src/research_os/manager.rs`
- Modify: `crates/ploy-research/src/research_os/mod.rs`
- Create: `crates/ploy-research/examples/factor_evolve_daily_plan.rs`
- Modify: `docs/ALPHA_FACTOR_SEARCH_CICD.md`

- [ ] **Step 1: Define typed input/output contracts**

Add structs:

```rust
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ResearchBudget {
    pub max_candidates_per_day: usize,
    pub max_backtests_per_day: usize,
    pub max_llm_calls_per_day: usize,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ResearchManagerInput {
    pub latest_runs: serde_json::Value,
    pub factor_registry_summary: serde_json::Value,
    pub rejected_factor_patterns: serde_json::Value,
    pub market_data_health: serde_json::Value,
    pub research_budget: ResearchBudget,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ResearchManagerPlan {
    pub theme: String,
    pub candidate_count: usize,
    pub search_depth: usize,
    pub priority: String,
    pub evidence_stage: String,
    pub actions: Vec<String>,
}
```

- [ ] **Step 2: Implement deterministic planner v0**

Planner v0 must not call an LLM. It should choose:

- `fix_data` if market data health contains stale/missing critical surfaces;
- `fix_runtime` if replay parity is missing for a ready handoff;
- `revise_prior` if alpha search stagnated;
- `continue_search` when MCTS plan has selected nodes and no promotion blockers require data/runtime fixes.

- [ ] **Step 3: Add example runner**

`factor_evolve_daily_plan.rs` reads a JSON input path and writes a JSON plan. It must fail closed if `evidence_stage` is not one of `diagnostic`, `factor_attribution`, `walk_forward`, `runtime_parity`, `dry_run_candidate`.

- [ ] **Step 4: Document boundary**

Update `docs/ALPHA_FACTOR_SEARCH_CICD.md`:

- Research Manager v0 is deterministic and typed.
- LLM integration is allowed only by producing a typed prior JSON.
- The manager cannot mutate evaluator thresholds, split policy, cost model, or promotion gate.

- [ ] **Step 5: Verify**

```bash
CARGO_TARGET_DIR=/tmp/ploy-factor-evolve-manager /opt/homebrew/bin/timeout 300 rtk cargo test --locked -p ploy-research research_os --lib
CARGO_TARGET_DIR=/tmp/ploy-factor-evolve-manager /opt/homebrew/bin/timeout 300 rtk cargo check --locked -p ploy-research --example factor_evolve_daily_plan
rtk git diff --check
```

- [ ] **Step 6: Commit**

```bash
git add crates/ploy-research/src/research_os docs/ALPHA_FACTOR_SEARCH_CICD.md crates/ploy-research/examples/factor_evolve_daily_plan.rs
git commit -m "research: add typed FactorEvolve manager plan surface"
```

## Task 5: Dry-Run Report Must Show Running Zero-Trade Candidates

**Files:**
- Modify: `scripts/report_dryrun_summary.py`
- Modify: `scripts/check_dryrun_report_contract.py`
- Create: `tests/test_dryrun_report_running_candidate.py`

- [ ] **Step 1: Add failing report contract test**

Create a fixture in the test with:

- one deployment row `desired_state=running`, `observed_state=running`;
- no closed trades for that deployment;
- expected output strategy row with `closed_trades=0`, `open_exposure=0`, and `activity_status=running_no_closed_trades`.

- [ ] **Step 2: Update report builder**

Modify report generation so every active/running deployment appears even if there are no order/fill rows. Do not synthesize profit or win-rate. Use explicit zero/null fields.

- [ ] **Step 3: Update contract checker**

Require:

- each running dry-run deployment is present in report `strategies`;
- missing running deployment fails with `missing_running_deployment_report_row`;
- zero-activity rows are valid only when `closed_trades == 0` and `open_exposure == 0`.

- [ ] **Step 4: Verify**

```bash
python3 -m unittest tests.test_dryrun_report_running_candidate tests.test_dryrun_report_contracts
python3 -m py_compile scripts/report_dryrun_summary.py scripts/check_dryrun_report_contract.py
rtk git diff --check
```

- [ ] **Step 5: Commit**

```bash
git add scripts/report_dryrun_summary.py scripts/check_dryrun_report_contract.py tests/test_dryrun_report_running_candidate.py
git commit -m "reports: show running dry-run candidates without trades"
```

## Task 6: Portfolio Builder V0

**Files:**
- Create: `crates/ploy-research/src/research_os/portfolio.rs`
- Modify: `crates/ploy-research/src/research_os/mod.rs`
- Create: `crates/ploy-research/examples/factor_portfolio_builder.rs`

- [ ] **Step 1: Define input contract**

Input JSON must include factor metrics with:

- factor name/hash;
- reward;
- IC/ICIR;
- test PnL or top bucket label;
- turnover proxy;
- full-depth entry fill rate;
- pairwise correlations.

- [ ] **Step 2: Implement greedy de-correlation v0**

Select factors by descending reward, rejecting candidates with:

- `max_corr_existing >= 0.70`;
- `top_bucket_full_depth_entry_fill_rate < 0.30`;
- non-positive marginal score after turnover/capacity penalty.

- [ ] **Step 3: Write output contract**

Output JSON:

- selected factors;
- rejected factors with reasons;
- aggregate expected reward;
- max pairwise correlation;
- promotion decision `continue`, `revise`, or `portfolio_candidate`.

- [ ] **Step 4: Verify**

```bash
CARGO_TARGET_DIR=/tmp/ploy-factor-evolve-portfolio /opt/homebrew/bin/timeout 300 rtk cargo test --locked -p ploy-research portfolio --lib
CARGO_TARGET_DIR=/tmp/ploy-factor-evolve-portfolio /opt/homebrew/bin/timeout 300 rtk cargo check --locked -p ploy-research --example factor_portfolio_builder
rtk git diff --check
```

- [ ] **Step 5: Commit**

```bash
git add crates/ploy-research/src/research_os/portfolio.rs crates/ploy-research/src/research_os/mod.rs crates/ploy-research/examples/factor_portfolio_builder.rs
git commit -m "research: add FactorEvolve portfolio builder v0"
```

## Task 7: External Data Surface Roadmap Gates

**Files:**
- Create: `docs/runbooks/factor-evolve-data-surfaces.md`
- Modify: `docs/PROJECT_SEMANTICS.md`
- Modify: `docs/ALPHA_FACTOR_SEARCH_CICD.md`

- [ ] **Step 1: Document current surfaces**

Record current status:

- Binance partial LOB: present, not sequence-correct local book.
- Binance aggTrade: present.
- Polymarket quote ticks: present.
- Polymarket full CLOB snapshots: present and archived to lake.
- Official settlement: present for PM5D evidence.
- OI/funding/liquidation/basis: not first-class.
- OKX/Bybit LOB: not first-class.

- [ ] **Step 2: Add fail-closed surface taxonomy**

Add explicit categories:

- `required_for_prediction`
- `required_for_execution`
- `optional_context`
- `missing_blocks_promotion`

- [ ] **Step 3: Verify docs**

```bash
rg -n "FactorEvolve|required_for_prediction|required_for_execution|missing_blocks_promotion" docs/PROJECT_SEMANTICS.md docs/ALPHA_FACTOR_SEARCH_CICD.md docs/runbooks/factor-evolve-data-surfaces.md
rtk git diff --check
```

- [ ] **Step 4: Commit**

```bash
git add docs/runbooks/factor-evolve-data-surfaces.md docs/PROJECT_SEMANTICS.md docs/ALPHA_FACTOR_SEARCH_CICD.md
git commit -m "docs: define FactorEvolve data surface gates"
```

## Task 8: CI Orchestrator For Daily Research Loop

**Files:**
- Create: `.github/workflows/factor-evolve-daily-research.yml`
- Modify: `docs/ALPHA_FACTOR_SEARCH_CICD.md`

- [ ] **Step 1: Add workflow skeleton**

Workflow inputs:

- `git_ref`, default `main`;
- `snapshot_run_id`, optional;
- `research_budget_json`;
- `run_mode`, one of `plan_only`, `search`, `promote_handoff`;
- `create_issue`, default `false`.

- [ ] **Step 2: Wire existing safe jobs only**

The first workflow must only orchestrate existing paths:

```text
restore or require snapshot
  -> factor_evolve_daily_plan
  -> factor-walk-forward hosted artifact if run_mode=search
  -> alpha_search_closed_loop_agent
  -> issue/comment artifact
```

It must not deploy and must not edit strategy config.

- [ ] **Step 3: Verify YAML and dry-run command composition**

```bash
ruby -e 'require "yaml"; YAML.load_file(".github/workflows/factor-evolve-daily-research.yml"); puts "ok"'
rg -n "deploy|systemctl|ployctl deployments resume|create_config_pr" .github/workflows/factor-evolve-daily-research.yml
rtk git diff --check
```

Expected: YAML parses; deploy/resume/config PR patterns are absent or commented as forbidden.

- [ ] **Step 4: Commit**

```bash
git add .github/workflows/factor-evolve-daily-research.yml docs/ALPHA_FACTOR_SEARCH_CICD.md
git commit -m "ci: add FactorEvolve daily research orchestrator"
```

## Implementation Order

1. Task 5 first if operator visibility is urgent: it fixes the remote-observed reporting gap without changing strategy logic.
2. Task 1 next: it gives every later refactor a durable registry/trace target.
3. Task 3 and Task 4: connect existing alpha-search artifacts to typed Research Manager planning.
4. Task 2: promote LOB data quality from snapshot diagnostics to sequence-correct research data.
5. Task 6: add portfolio selection after individual factors are traceable.
6. Task 7: document and enforce missing external data surfaces.
7. Task 8: create the daily orchestrator only after the contracts are stable.

## Non-Goals For This Refactor

- No automatic live trading.
- No LLM permission to edit evaluator splits, labels, costs, or promotion thresholds.
- No local DB-backed research runs.
- No Rust builds on `tango-1-1`.
- No broad crate reshuffle outside the listed files unless a task fails because of current module boundaries.

## Verification Before Done

- Local:
  - `python3 -m unittest tests.test_factor_research_os_registry tests.test_dryrun_report_running_candidate`
  - `CARGO_TARGET_DIR=/tmp/ploy-factor-evolve /opt/homebrew/bin/timeout 300 rtk cargo test --locked -p ploy-research research_os orderbook autofactor alpha_search --lib`
  - `ruby -e 'require "yaml"; Dir[".github/workflows/*.yml"].each { |f| YAML.load_file(f) }'`
  - `rtk git diff --check`
- Remote:
  - `gh run list --workflow 'Factor Walk-Forward V2 Hosted Artifact' --limit 3`
  - `gh run list --workflow 'Runtime Candidate Replay' --limit 3`
  - `ssh tango-1-1 'systemctl is-active ployd.service && curl -fsS http://127.0.0.1:8081/health'`
  - `curl -fsS --max-time 30 http://8.221.143.151/api/reports/dry-run` and confirm the running settlement dry-run appears even if it has zero recent closed trades.

## Self-Review

- Spec coverage: The plan covers PRD registry/trace, constrained DSL, search tree artifacts, deterministic evaluator boundary, dry-run feedback, data surfaces, and Research Manager planning. The first implementation intentionally does not include OKX/Bybit/OI/funding collectors; it adds gates and roadmap docs before collectors.
- Placeholder scan: No task depends on undefined "later" behavior; each task has concrete files, commands, and acceptance checks.
- Type consistency: `FactorLifecycleStatus`, `FactorRegistryEntry`, `ResearchManagerInput`, `ResearchManagerPlan`, and `trace_hash` are defined before later tasks consume them.
