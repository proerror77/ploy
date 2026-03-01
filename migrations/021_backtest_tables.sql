-- Migration: 021_backtest_tables
-- Purpose: Backtest run metadata, signals, and closed trades for the directional strategy

-- 1. Backtest run metadata
CREATE TABLE backtest_runs (
    run_id        UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    strategy      TEXT NOT NULL,
    mode          TEXT NOT NULL,
    config_json   JSONB NOT NULL,
    symbols       TEXT[] NOT NULL,
    data_start    TIMESTAMPTZ,
    data_end      TIMESTAMPTZ,
    total_trades  INT,
    win_rate      DOUBLE PRECISION,
    total_pnl     NUMERIC,
    sharpe_ratio  DOUBLE PRECISION,
    max_drawdown  NUMERIC,
    profit_factor DOUBLE PRECISION,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- 2. Backtest signals (entry + exit + filtered)
CREATE TABLE backtest_signals (
    id            BIGSERIAL PRIMARY KEY,
    run_id        UUID NOT NULL REFERENCES backtest_runs(run_id),
    signal_type   TEXT NOT NULL,          -- 'entry', 'exit', 'filtered'
    symbol        TEXT NOT NULL,
    direction     TEXT NOT NULL,          -- 'UP', 'DOWN'
    timestamp     TIMESTAMPTZ NOT NULL,
    p_hat         DOUBLE PRECISION,
    ev_net        DOUBLE PRECISION,
    sigma         DOUBLE PRECISION,
    market_price  NUMERIC,
    spot_price    NUMERIC,
    s0            NUMERIC,
    time_remaining_secs DOUBLE PRECISION,
    filter_reason TEXT,                   -- only for 'filtered': cooldown, max_positions, price_bounds, etc.
    exit_reason   TEXT,                   -- only for 'exit': settlement, time_stop, hard_stop, prob_stop
    exit_price    NUMERIC,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX idx_bt_signals_run ON backtest_signals(run_id);

-- 3. Backtest closed trades
CREATE TABLE backtest_trades (
    id            BIGSERIAL PRIMARY KEY,
    run_id        UUID NOT NULL REFERENCES backtest_runs(run_id),
    symbol        TEXT NOT NULL,
    direction     TEXT NOT NULL,
    entry_time    TIMESTAMPTZ NOT NULL,
    exit_time     TIMESTAMPTZ NOT NULL,
    entry_price   NUMERIC NOT NULL,
    exit_price    NUMERIC NOT NULL,
    shares        INT NOT NULL,
    pnl           NUMERIC NOT NULL,
    won           BOOLEAN NOT NULL,
    holding_secs  BIGINT NOT NULL,
    exit_reason   TEXT NOT NULL,
    entry_p_hat   DOUBLE PRECISION,
    entry_ev_net  DOUBLE PRECISION,
    entry_sigma   DOUBLE PRECISION,
    s0            NUMERIC,
    -- Gamma settlement dual verification
    gamma_settled_price NUMERIC,
    gamma_resolved      BOOLEAN,
    gamma_match         BOOLEAN,          -- tick outcome vs Gamma agreement
    created_at    TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX idx_bt_trades_run ON backtest_trades(run_id);
