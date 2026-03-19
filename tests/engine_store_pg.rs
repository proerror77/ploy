use chrono::{Duration as ChronoDuration, Utc};
use ploy::adapters::PostgresStore;
use ploy::domain::{Round, Side, StrategyState};
use ploy::error::PloyError;
use rust_decimal_macros::dec;
use sqlx::postgres::{PgPool, PgPoolOptions};
use sqlx::Row;
use std::env;
use std::process::Command;
use std::time::{Duration, Instant};
use uuid::Uuid;

struct DockerPostgres {
    name: String,
    database_url: String,
}

impl DockerPostgres {
    async fn start() -> Option<Self> {
        if !Self::docker_available() {
            return None;
        }

        let name = format!("ploy-engine-store-it-{}", Uuid::new_v4().simple());
        let output = Command::new("docker")
            .args([
                "run",
                "-d",
                "--rm",
                "--name",
                &name,
                "-e",
                "POSTGRES_USER=postgres",
                "-e",
                "POSTGRES_PASSWORD=postgres",
                "-e",
                "POSTGRES_DB=ploy_test",
                "-P",
                "postgres:16-alpine",
            ])
            .output()
            .expect("failed to start postgres test container");

        if !output.status.success() {
            panic!(
                "failed to start postgres test container: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }

        let deadline = Instant::now() + Duration::from_secs(30);
        let port = loop {
            if let Some(port) = Self::resolve_host_port(&name) {
                break port;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for docker port mapping"
            );
            tokio::time::sleep(Duration::from_millis(200)).await;
        };

        let database_url = format!("postgres://postgres:postgres@127.0.0.1:{port}/ploy_test");

        let deadline = Instant::now() + Duration::from_secs(45);
        loop {
            match PgPoolOptions::new()
                .max_connections(1)
                .connect(&database_url)
                .await
            {
                Ok(pool) => {
                    pool.close().await;
                    break;
                }
                Err(_) if Instant::now() < deadline => {
                    tokio::time::sleep(Duration::from_millis(300)).await;
                }
                Err(err) => panic!("timed out waiting for postgres readiness: {err}"),
            }
        }

        Some(Self { name, database_url })
    }

    fn docker_available() -> bool {
        Command::new("docker")
            .arg("info")
            .output()
            .ok()
            .map(|output| output.status.success())
            .unwrap_or(false)
    }

    fn resolve_host_port(name: &str) -> Option<u16> {
        let output = Command::new("docker")
            .args(["port", name, "5432/tcp"])
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        stdout.lines().find_map(|line| {
            line.rsplit(':')
                .next()
                .and_then(|raw| raw.trim().parse::<u16>().ok())
        })
    }
}

impl Drop for DockerPostgres {
    fn drop(&mut self) {
        let _ = Command::new("docker")
            .args(["rm", "-f", &self.name])
            .status();
    }
}

struct TestDb {
    store: PostgresStore,
    _docker: Option<DockerPostgres>,
}

async fn ensure_engine_store_schema(pool: &PgPool) {
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS rounds (
            id SERIAL PRIMARY KEY,
            slug TEXT NOT NULL UNIQUE,
            up_token_id TEXT NOT NULL,
            down_token_id TEXT NOT NULL,
            start_time TIMESTAMPTZ NOT NULL,
            end_time TIMESTAMPTZ NOT NULL,
            outcome TEXT
        )
        "#,
    )
    .execute(pool)
    .await
    .expect("failed to create rounds table for integration test");

    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS cycles (
            id SERIAL PRIMARY KEY,
            round_id INTEGER NOT NULL REFERENCES rounds(id),
            state TEXT NOT NULL,
            leg1_side TEXT,
            leg1_entry_price DECIMAL,
            leg1_shares INTEGER,
            leg1_filled_at TIMESTAMPTZ,
            leg2_entry_price DECIMAL,
            leg2_shares INTEGER,
            leg2_filled_at TIMESTAMPTZ,
            pnl DECIMAL,
            abort_reason TEXT,
            version INTEGER NOT NULL DEFAULT 1,
            created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
            updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
        )
        "#,
    )
    .execute(pool)
    .await
    .expect("failed to create cycles table for integration test");
}

impl TestDb {
    async fn new() -> Option<Self> {
        let (docker, database_url) = if let Ok(url) = env::var("PLOY_TEST_DATABASE_URL") {
            (None, url)
        } else if let Some(docker) = DockerPostgres::start().await {
            let url = docker.database_url.clone();
            (Some(docker), url)
        } else {
            eprintln!(
                "Skipping engine_store_pg integration test: configure docker daemon or PLOY_TEST_DATABASE_URL"
            );
            return None;
        };

        let pool = PgPoolOptions::new()
            .max_connections(5)
            .connect(&database_url)
            .await
            .expect("failed to connect postgres test database");

        ensure_engine_store_schema(&pool).await;

        let store = PostgresStore::from_pool(pool);

        Some(Self {
            store,
            _docker: docker,
        })
    }
}

async fn create_round_and_cycle(store: &PostgresStore, tag: &str) -> i32 {
    let now = Utc::now();
    let uid = Uuid::new_v4().simple().to_string();
    let round = Round {
        id: None,
        slug: format!("it-{tag}-{uid}"),
        up_token_id: format!("up-{uid}"),
        down_token_id: format!("down-{uid}"),
        start_time: now - ChronoDuration::minutes(5),
        end_time: now + ChronoDuration::minutes(10),
        outcome: None,
    };

    let round_id = store
        .upsert_round(&round)
        .await
        .expect("upsert_round should succeed");

    store
        .create_cycle(round_id, StrategyState::Leg1Pending)
        .await
        .expect("create_cycle should succeed")
}

async fn cycle_version(store: &PostgresStore, cycle_id: i32) -> i32 {
    sqlx::query("SELECT version FROM cycles WHERE id = $1")
        .bind(cycle_id)
        .fetch_one(store.pool())
        .await
        .expect("query cycle version should succeed")
        .get("version")
}

async fn cycle_state(store: &PostgresStore, cycle_id: i32) -> String {
    sqlx::query("SELECT state FROM cycles WHERE id = $1")
        .bind(cycle_id)
        .fetch_one(store.pool())
        .await
        .expect("query cycle state should succeed")
        .get("state")
}

fn assert_version_conflict(err: PloyError, cycle_id: i32, expected_version: i32) {
    match err {
        PloyError::VersionConflict {
            entity,
            id,
            expected_version: got,
        } => {
            assert_eq!(entity, "cycle", "conflict entity should be cycle");
            assert_eq!(id, cycle_id, "conflict cycle id should match");
            assert_eq!(
                got, expected_version,
                "conflict should expose expected version"
            );
        }
        other => panic!("expected VersionConflict, got: {other:?}"),
    }
}

#[tokio::test]
async fn update_cycle_state_success_increments_version() {
    let Some(ctx) = TestDb::new().await else {
        return;
    };

    let cycle_id = create_round_and_cycle(&ctx.store, "state-success").await;

    assert_eq!(cycle_version(&ctx.store, cycle_id).await, 1);

    ctx.store
        .update_cycle_state(cycle_id, StrategyState::Leg1Filled, 1)
        .await
        .expect("update_cycle_state should succeed with matching version");

    assert_eq!(cycle_version(&ctx.store, cycle_id).await, 2);
    assert_eq!(cycle_state(&ctx.store, cycle_id).await, "LEG1_FILLED");
}

#[tokio::test]
async fn update_cycle_state_conflict_returns_version_conflict() {
    let Some(ctx) = TestDb::new().await else {
        return;
    };

    let cycle_id = create_round_and_cycle(&ctx.store, "state-conflict").await;

    let err = ctx
        .store
        .update_cycle_state(cycle_id, StrategyState::Leg1Filled, 0)
        .await
        .expect_err("stale expected version should conflict");

    assert_version_conflict(err, cycle_id, 0);
    assert_eq!(cycle_version(&ctx.store, cycle_id).await, 1);
    assert_eq!(cycle_state(&ctx.store, cycle_id).await, "LEG1_PENDING");
}

#[tokio::test]
async fn update_cycle_leg1_conflict_returns_version_conflict() {
    let Some(ctx) = TestDb::new().await else {
        return;
    };

    let cycle_id = create_round_and_cycle(&ctx.store, "leg1-conflict").await;

    let err = ctx
        .store
        .update_cycle_leg1(cycle_id, Side::Up, dec!(0.42), 100, 0)
        .await
        .expect_err("stale expected version should conflict");

    assert_version_conflict(err, cycle_id, 0);
    assert_eq!(cycle_version(&ctx.store, cycle_id).await, 1);
    assert_eq!(cycle_state(&ctx.store, cycle_id).await, "LEG1_PENDING");
}

#[tokio::test]
async fn update_cycle_leg2_conflict_returns_version_conflict() {
    let Some(ctx) = TestDb::new().await else {
        return;
    };

    let cycle_id = create_round_and_cycle(&ctx.store, "leg2-conflict").await;

    // Move cycle to version=2 first.
    ctx.store
        .update_cycle_leg1(cycle_id, Side::Up, dec!(0.40), 120, 1)
        .await
        .expect("leg1 update should succeed before leg2 stale-check");

    let err = ctx
        .store
        .update_cycle_leg2(cycle_id, dec!(0.55), 120, dec!(0.06), 1)
        .await
        .expect_err("stale leg2 expected version should conflict");

    assert_version_conflict(err, cycle_id, 1);
    assert_eq!(cycle_version(&ctx.store, cycle_id).await, 2);
    assert_eq!(cycle_state(&ctx.store, cycle_id).await, "LEG1_FILLED");
}

#[tokio::test]
async fn concurrent_cycle_updates_yield_one_success_and_one_conflict() {
    let Some(ctx) = TestDb::new().await else {
        return;
    };

    let cycle_id = create_round_and_cycle(&ctx.store, "concurrent").await;

    let s1 = ctx.store.clone();
    let s2 = ctx.store.clone();

    let t1 = tokio::spawn(async move {
        s1.update_cycle_state(cycle_id, StrategyState::Leg1Filled, 1)
            .await
    });
    let t2 = tokio::spawn(async move {
        s2.update_cycle_state(cycle_id, StrategyState::Abort, 1).await
    });

    let r1 = t1.await.expect("join should succeed");
    let r2 = t2.await.expect("join should succeed");

    let mut success = 0;
    let mut conflicts = 0;

    for result in [r1, r2] {
        match result {
            Ok(()) => success += 1,
            Err(PloyError::VersionConflict { .. }) => conflicts += 1,
            Err(other) => panic!("unexpected concurrent update result: {other:?}"),
        }
    }

    assert_eq!(success, 1, "exactly one update should succeed");
    assert_eq!(conflicts, 1, "exactly one update should conflict");
    assert_eq!(cycle_version(&ctx.store, cycle_id).await, 2);

    let state = cycle_state(&ctx.store, cycle_id).await;
    assert!(
        state == "LEG1_FILLED" || state == "ABORT",
        "final state should reflect the successful winner"
    );
}
