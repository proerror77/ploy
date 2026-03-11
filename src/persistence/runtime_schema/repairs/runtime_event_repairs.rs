pub(super) const SQL: &str = r#"
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
"#;
