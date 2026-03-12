pub(super) const SQL: &str = r#"
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
"#;
