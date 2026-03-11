use crate::error::Result;
use sqlx::PgPool;
use tracing::warn;

pub(crate) async fn ensure_schema_repairs(pool: &PgPool) -> Result<()> {
    let result = sqlx::query(
        r#"
        DO $$
        BEGIN
            BEGIN
                IF to_regclass('public.orders') IS NOT NULL THEN
                    EXECUTE 'DROP INDEX IF EXISTS idx_orders_cycle_leg';
                    EXECUTE 'CREATE INDEX IF NOT EXISTS idx_orders_cycle_leg ON orders(cycle_id, leg, created_at DESC) WHERE cycle_id IS NOT NULL';
                END IF;
            EXCEPTION WHEN insufficient_privilege THEN
                NULL;
            END;

            BEGIN
                IF to_regclass('public.positions') IS NOT NULL THEN
                    EXECUTE 'DROP INDEX IF EXISTS idx_positions_status_opened';
                    IF EXISTS (
                        SELECT 1
                        FROM information_schema.columns
                        WHERE table_schema = 'public'
                          AND table_name = 'positions'
                          AND column_name = 'opened_at'
                    ) THEN
                        EXECUTE 'CREATE INDEX IF NOT EXISTS idx_positions_status_opened ON positions(status, opened_at DESC) WHERE status = ''OPEN''';
                    END IF;
                END IF;
            EXCEPTION WHEN insufficient_privilege THEN
                NULL;
            END;

            BEGIN
                IF to_regclass('public.positions') IS NOT NULL THEN
                    EXECUTE 'ALTER TABLE positions ADD COLUMN IF NOT EXISTS account_id TEXT NOT NULL DEFAULT ''default''';
                    EXECUTE 'ALTER TABLE positions DROP CONSTRAINT IF EXISTS positions_event_id_token_id_key';
                    EXECUTE 'CREATE UNIQUE INDEX IF NOT EXISTS idx_positions_account_event_token_unique ON positions(account_id, event_id, token_id)';

                    IF EXISTS (
                        SELECT 1
                        FROM information_schema.columns
                        WHERE table_schema = 'public'
                          AND table_name = 'positions'
                          AND column_name = 'opened_at'
                    ) THEN
                        EXECUTE 'CREATE INDEX IF NOT EXISTS idx_positions_account_status_opened ON positions(account_id, status, opened_at DESC) WHERE status = ''OPEN''';
                    END IF;
                END IF;
            EXCEPTION WHEN insufficient_privilege THEN
                NULL;
            END;

            BEGIN
                IF to_regclass('public.position_reconciliation_log') IS NOT NULL THEN
                    EXECUTE 'DROP INDEX IF EXISTS idx_reconciliation_log_created';
                    IF EXISTS (
                        SELECT 1
                        FROM information_schema.columns
                        WHERE table_schema = 'public'
                          AND table_name = 'position_reconciliation_log'
                          AND column_name = 'timestamp'
                    ) THEN
                        EXECUTE 'CREATE INDEX IF NOT EXISTS idx_reconciliation_log_created ON position_reconciliation_log(timestamp DESC)';
                    END IF;
                END IF;
            EXCEPTION WHEN insufficient_privilege THEN
                NULL;
            END;

            BEGIN
                IF to_regclass('public.position_discrepancies') IS NOT NULL THEN
                    EXECUTE 'CREATE INDEX IF NOT EXISTS idx_discrepancies_severity_unresolved ON position_discrepancies(severity, created_at DESC) WHERE resolved = FALSE';
                END IF;
            EXCEPTION WHEN insufficient_privilege THEN
                NULL;
            END;

            BEGIN
                IF to_regclass('public.nonce_usage') IS NOT NULL THEN
                    EXECUTE 'DROP INDEX IF EXISTS idx_nonce_usage_active';
                    IF EXISTS (
                        SELECT 1
                        FROM information_schema.columns
                        WHERE table_schema = 'public'
                          AND table_name = 'nonce_usage'
                          AND column_name = 'wallet_address'
                    ) AND EXISTS (
                        SELECT 1
                        FROM information_schema.columns
                        WHERE table_schema = 'public'
                          AND table_name = 'nonce_usage'
                          AND column_name = 'released_at'
                    ) AND EXISTS (
                        SELECT 1
                        FROM information_schema.columns
                        WHERE table_schema = 'public'
                          AND table_name = 'nonce_usage'
                          AND column_name = 'allocated_at'
                    ) THEN
                        EXECUTE 'CREATE INDEX IF NOT EXISTS idx_nonce_usage_active ON nonce_usage(wallet_address, allocated_at DESC) WHERE released_at IS NULL';
                    ELSIF EXISTS (
                        SELECT 1
                        FROM information_schema.columns
                        WHERE table_schema = 'public'
                          AND table_name = 'nonce_usage'
                          AND column_name = 'wallet_address'
                    ) AND EXISTS (
                        SELECT 1
                        FROM information_schema.columns
                        WHERE table_schema = 'public'
                          AND table_name = 'nonce_usage'
                          AND column_name = 'released_at'
                    ) AND EXISTS (
                        SELECT 1
                        FROM information_schema.columns
                        WHERE table_schema = 'public'
                          AND table_name = 'nonce_usage'
                          AND column_name = 'used_at'
                    ) THEN
                        EXECUTE 'CREATE INDEX IF NOT EXISTS idx_nonce_usage_active ON nonce_usage(wallet_address, used_at DESC) WHERE released_at IS NULL';
                    END IF;
                END IF;
            EXCEPTION WHEN insufficient_privilege THEN
                NULL;
            END;

            BEGIN
                IF to_regclass('public.fills') IS NOT NULL THEN
                    EXECUTE 'DROP INDEX IF EXISTS idx_fills_position_time';
                    EXECUTE 'DROP INDEX IF EXISTS idx_fills_order_time';
                    IF EXISTS (
                        SELECT 1
                        FROM information_schema.columns
                        WHERE table_schema = 'public'
                          AND table_name = 'fills'
                          AND column_name = 'timestamp'
                    ) THEN
                        EXECUTE 'CREATE INDEX IF NOT EXISTS idx_fills_position_time ON fills(position_id, timestamp DESC)';
                        EXECUTE 'CREATE INDEX IF NOT EXISTS idx_fills_order_time ON fills(order_id, timestamp DESC)';
                    ELSIF EXISTS (
                        SELECT 1
                        FROM information_schema.columns
                        WHERE table_schema = 'public'
                          AND table_name = 'fills'
                          AND column_name = 'filled_at'
                    ) THEN
                        EXECUTE 'CREATE INDEX IF NOT EXISTS idx_fills_position_time ON fills(position_id, filled_at DESC)';
                        EXECUTE 'CREATE INDEX IF NOT EXISTS idx_fills_order_time ON fills(order_id, filled_at DESC)';
                    END IF;
                END IF;
            EXCEPTION WHEN insufficient_privilege THEN
                NULL;
            END;

            BEGIN
                IF to_regclass('public.balance_snapshots') IS NOT NULL THEN
                    EXECUTE 'DROP INDEX IF EXISTS idx_balance_snapshots_latest';
                    IF EXISTS (
                        SELECT 1
                        FROM information_schema.columns
                        WHERE table_schema = 'public'
                          AND table_name = 'balance_snapshots'
                          AND column_name = 'timestamp'
                    ) THEN
                        EXECUTE 'CREATE INDEX IF NOT EXISTS idx_balance_snapshots_latest ON balance_snapshots(timestamp DESC)';
                    ELSIF EXISTS (
                        SELECT 1
                        FROM information_schema.columns
                        WHERE table_schema = 'public'
                          AND table_name = 'balance_snapshots'
                          AND column_name = 'created_at'
                    ) THEN
                        EXECUTE 'CREATE INDEX IF NOT EXISTS idx_balance_snapshots_latest ON balance_snapshots(created_at DESC)';
                    END IF;
                END IF;
            EXCEPTION WHEN insufficient_privilege THEN
                NULL;
            END;

            BEGIN
                IF to_regclass('public.component_heartbeats') IS NOT NULL THEN
                    EXECUTE 'DROP INDEX IF EXISTS idx_heartbeats_component_time';
                    IF EXISTS (
                        SELECT 1
                        FROM information_schema.columns
                        WHERE table_schema = 'public'
                          AND table_name = 'component_heartbeats'
                          AND column_name = 'component_name'
                    ) THEN
                        EXECUTE 'CREATE INDEX IF NOT EXISTS idx_heartbeats_component_time ON component_heartbeats(component_name, last_heartbeat DESC)';
                    ELSIF EXISTS (
                        SELECT 1
                        FROM information_schema.columns
                        WHERE table_schema = 'public'
                          AND table_name = 'component_heartbeats'
                          AND column_name = 'component'
                    ) THEN
                        EXECUTE 'CREATE INDEX IF NOT EXISTS idx_heartbeats_component_time ON component_heartbeats(component, last_heartbeat DESC)';
                    END IF;
                END IF;
            EXCEPTION WHEN insufficient_privilege THEN
                NULL;
            END;

            BEGIN
                IF to_regclass('public.system_events') IS NOT NULL THEN
                    EXECUTE 'DROP INDEX IF EXISTS idx_system_events_component_time';
                    IF EXISTS (
                        SELECT 1
                        FROM information_schema.columns
                        WHERE table_schema = 'public'
                          AND table_name = 'system_events'
                          AND column_name = 'component'
                    ) THEN
                        EXECUTE 'CREATE INDEX IF NOT EXISTS idx_system_events_component_time ON system_events(component, created_at DESC)';
                    END IF;
                END IF;
            EXCEPTION WHEN insufficient_privilege THEN
                NULL;
            END;

            BEGIN
                IF to_regclass('public.order_idempotency') IS NOT NULL THEN
                    EXECUTE 'ALTER TABLE order_idempotency ADD COLUMN IF NOT EXISTS account_id TEXT NOT NULL DEFAULT ''default''';
                    EXECUTE 'ALTER TABLE order_idempotency ADD COLUMN IF NOT EXISTS request_hash TEXT';
                    EXECUTE 'ALTER TABLE order_idempotency ADD COLUMN IF NOT EXISTS response_data JSONB';
                    EXECUTE 'ALTER TABLE order_idempotency ADD COLUMN IF NOT EXISTS error_message TEXT';
                    EXECUTE 'ALTER TABLE order_idempotency ADD COLUMN IF NOT EXISTS updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()';
                    EXECUTE 'ALTER TABLE order_idempotency DROP CONSTRAINT IF EXISTS order_idempotency_idempotency_key_key';

                    IF EXISTS (
                        SELECT 1
                        FROM pg_constraint c
                        JOIN unnest(c.conkey) WITH ORDINALITY AS x(attnum, ordinality) ON true
                        JOIN pg_attribute a ON a.attrelid = c.conrelid AND a.attnum = x.attnum
                        WHERE c.conrelid = 'public.order_idempotency'::regclass
                          AND c.contype = 'p'
                        GROUP BY c.oid
                        HAVING array_agg(a.attname::text ORDER BY x.ordinality) = ARRAY['idempotency_key']::text[]
                    ) THEN
                        EXECUTE 'ALTER TABLE order_idempotency DROP CONSTRAINT order_idempotency_pkey';
                        EXECUTE 'ALTER TABLE order_idempotency ADD PRIMARY KEY (account_id, idempotency_key)';
                    ELSE
                        EXECUTE 'CREATE UNIQUE INDEX IF NOT EXISTS idx_order_idempotency_account_key ON order_idempotency(account_id, idempotency_key)';
                    END IF;

                    EXECUTE 'CREATE INDEX IF NOT EXISTS idx_order_idempotency_key ON order_idempotency(idempotency_key)';
                    EXECUTE 'CREATE INDEX IF NOT EXISTS idx_order_idempotency_hash ON order_idempotency(request_hash)';
                    EXECUTE 'CREATE INDEX IF NOT EXISTS idx_order_idempotency_status ON order_idempotency(status, created_at)';
                    EXECUTE 'CREATE INDEX IF NOT EXISTS idx_order_idempotency_expires ON order_idempotency(expires_at)';
                    EXECUTE 'CREATE INDEX IF NOT EXISTS idx_order_idempotency_account_expires ON order_idempotency(account_id, expires_at)';

                    IF EXISTS (
                        SELECT 1
                        FROM pg_proc
                        WHERE proname = 'update_updated_at_column'
                          AND pg_function_is_visible(oid)
                    ) THEN
                        EXECUTE 'DROP TRIGGER IF EXISTS update_order_idempotency_updated_at ON order_idempotency';
                        EXECUTE 'CREATE TRIGGER update_order_idempotency_updated_at BEFORE UPDATE ON order_idempotency FOR EACH ROW EXECUTE FUNCTION update_updated_at_column()';
                    END IF;
                END IF;
            EXCEPTION WHEN insufficient_privilege THEN
                NULL;
            END;

            BEGIN
                IF to_regclass('public.quote_freshness') IS NOT NULL THEN
                    IF NOT EXISTS (
                        SELECT 1
                        FROM information_schema.columns
                        WHERE table_schema = 'public'
                          AND table_name = 'quote_freshness'
                          AND column_name = 'is_stale'
                    ) THEN
                        EXECUTE 'ALTER TABLE quote_freshness ADD COLUMN is_stale BOOLEAN NOT NULL DEFAULT FALSE';
                    END IF;

                    IF EXISTS (
                        SELECT 1
                        FROM information_schema.columns
                        WHERE table_schema = 'public'
                          AND table_name = 'quote_freshness'
                          AND column_name = 'is_stale'
                          AND is_generated = 'NEVER'
                    ) AND EXISTS (
                        SELECT 1
                        FROM information_schema.columns
                        WHERE table_schema = 'public'
                          AND table_name = 'quote_freshness'
                          AND column_name = 'received_at'
                    ) THEN
                        EXECUTE 'UPDATE quote_freshness SET is_stale = (EXTRACT(EPOCH FROM (NOW() - received_at)) > 30) WHERE is_stale IS DISTINCT FROM (EXTRACT(EPOCH FROM (NOW() - received_at)) > 30)';
                    END IF;

                    EXECUTE 'CREATE INDEX IF NOT EXISTS idx_quote_freshness_stale ON quote_freshness(is_stale) WHERE is_stale = false';
                END IF;
            EXCEPTION WHEN insufficient_privilege THEN
                NULL;
            END;
        END $$;
        "#,
    )
    .execute(pool)
    .await;

    if let Err(e) = result {
        warn!(
            error = %e,
            "schema repair DDL skipped at startup (run migration 013 as postgres for full repair)"
        );
    }

    Ok(())
}
