#![cfg(feature = "api")]

use axum::{
    body::{to_bytes, Body},
    http::{Method, Request, StatusCode},
    Router,
};
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use ploy::{
    adapters::PostgresStore,
    api::{create_router, state::StrategyConfigState, AppState},
};
use serde_json::{json, Value};
use sqlx::{postgres::PgPoolOptions, PgPool};
use std::{
    env,
    process::Command,
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};
use tower::ServiceExt;
use uuid::Uuid;

static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

fn env_lock() -> &'static Mutex<()> {
    ENV_LOCK.get_or_init(|| Mutex::new(()))
}

#[derive(Default)]
struct EnvOverride {
    previous: Vec<(String, Option<String>)>,
}

impl EnvOverride {
    fn set(&mut self, key: &str, value: &str) {
        if !self.previous.iter().any(|(existing, _)| existing == key) {
            self.previous.push((key.to_string(), env::var(key).ok()));
        }
        unsafe { env::set_var(key, value) };
    }
}

impl Drop for EnvOverride {
    fn drop(&mut self) {
        for (key, value) in self.previous.iter().rev() {
            if let Some(value) = value {
                unsafe { env::set_var(key, value) };
            } else {
                unsafe { env::remove_var(key) };
            }
        }
    }
}

struct DockerPostgres {
    name: String,
    database_url: String,
}

impl DockerPostgres {
    async fn start() -> Option<Self> {
        if !Self::docker_available() {
            eprintln!("Skipping integration test: docker is not available");
            return None;
        }

        let name = format!("ploy-api-it-{}", Uuid::new_v4().simple());
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
                Err(err) => {
                    panic!("timed out waiting for postgres readiness: {err}");
                }
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

struct TestContext {
    app: Router,
    pool: PgPool,
    _docker: Option<DockerPostgres>,
    _env: EnvOverride,
}

impl TestContext {
    async fn new(extra_env: &[(&str, &str)]) -> Option<Self> {
        let mut env_override = EnvOverride::default();
        env_override.set("PLOY_ACCOUNT_ID", "default");
        env_override.set("PLOY_DRY_RUN__ENABLED", "true");
        env_override.set("PLOY_API_ADMIN_AUTH_REQUIRED", "true");
        env_override.set("PLOY_API_ADMIN_TOKEN", "admin-test-token");
        env_override.set("PLOY_SIDECAR_AUTH_REQUIRED", "true");
        env_override.set("PLOY_SIDECAR_AUTH_TOKEN", "sidecar-test-token");
        env_override.set(
            "PLOY_DEPLOYMENTS_FILE",
            &format!(
                "{}/ploy-deployments-{}.json",
                env::temp_dir().display(),
                Uuid::new_v4().simple()
            ),
        );
        env_override.set("PLOY_STRATEGY_DEPLOYMENTS_JSON", "[]");

        for (key, value) in extra_env {
            env_override.set(key, value);
        }

        let (docker, database_url) = if let Some(docker) = DockerPostgres::start().await {
            let url = docker.database_url.clone();
            (Some(docker), url)
        } else if let Ok(url) = env::var("PLOY_TEST_DATABASE_URL") {
            (None, url)
        } else {
            eprintln!(
                "Skipping integration test: configure docker daemon or PLOY_TEST_DATABASE_URL"
            );
            return None;
        };

        let pool = PgPoolOptions::new()
            .max_connections(5)
            .connect(&database_url)
            .await
            .expect("failed to connect postgres test database");

        ensure_strategy_evaluations_table(&pool).await;

        let store = Arc::new(PostgresStore::from_pool(pool.clone()));
        let config = StrategyConfigState {
            symbols: vec!["BTC".to_string()],
            min_move: 0.0,
            max_entry: 1.0,
            shares: 1,
            predictive: false,
            exit_edge_floor: None,
            exit_price_band: None,
            time_decay_exit_secs: None,
            liquidity_exit_spread_bps: None,
        };
        let state = AppState::new(store, config);
        let app = create_router(state);

        Some(Self {
            app,
            pool,
            _docker: docker,
            _env: env_override,
        })
    }
}

async fn ensure_strategy_evaluations_table(pool: &PgPool) {
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS strategy_evaluations (
            id BIGSERIAL PRIMARY KEY,
            account_id TEXT NOT NULL DEFAULT 'default',
            evaluated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
            strategy_id TEXT NOT NULL,
            deployment_id TEXT,
            domain TEXT NOT NULL,
            stage TEXT NOT NULL CHECK (stage IN ('BACKTEST','PAPER','LIVE')),
            status TEXT NOT NULL CHECK (status IN ('PASS','FAIL','WARN','UNKNOWN')),
            score NUMERIC(12,6),
            timeframe TEXT,
            sample_size BIGINT,
            pnl_usd NUMERIC(20,10),
            win_rate NUMERIC(12,6),
            sharpe NUMERIC(20,10),
            max_drawdown_pct NUMERIC(12,6),
            max_drawdown_usd NUMERIC(20,10),
            evidence_kind TEXT NOT NULL DEFAULT 'report',
            evidence_ref TEXT,
            evidence_hash TEXT,
            evidence_payload JSONB,
            metadata JSONB
        )
        "#,
    )
    .execute(pool)
    .await
    .expect("failed to create strategy_evaluations table");

    sqlx::query(
        r#"
        CREATE UNIQUE INDEX IF NOT EXISTS idx_strategy_evaluations_evidence_hash
            ON strategy_evaluations(account_id, strategy_id, stage, evidence_hash)
            WHERE evidence_hash IS NOT NULL
        "#,
    )
    .execute(pool)
    .await
    .expect("failed to create strategy_evaluations unique index");
}

async fn insert_evaluation(
    pool: &PgPool,
    strategy_id: &str,
    stage: &str,
    status: &str,
    evaluated_at: DateTime<Utc>,
) {
    sqlx::query(
        r#"
        INSERT INTO strategy_evaluations (
            account_id,
            evaluated_at,
            strategy_id,
            deployment_id,
            domain,
            stage,
            status,
            evidence_kind
        )
        VALUES ($1, $2, $3, NULL, $4, $5, $6, $7)
        "#,
    )
    .bind("default")
    .bind(evaluated_at)
    .bind(strategy_id)
    .bind("CRYPTO")
    .bind(stage)
    .bind(status)
    .bind("report")
    .execute(pool)
    .await
    .expect("failed to insert strategy evaluation");
}

async fn upsert_disabled_deployment(app: &Router, deployment_id: &str, strategy_id: &str) {
    let payload = json!({
        "replace": true,
        "deployments": [{
            "id": deployment_id,
            "strategy": strategy_id,
            "domain": "crypto",
            "market_selector": {
                "mode": "static",
                "market_slug": "btc-up-or-down-5m"
            },
            "timeframe": "5m",
            "enabled": false,
            "allocator_profile": "default",
            "risk_profile": "default",
            "priority": 1,
            "cooldown_secs": 0,
            "account_ids": [],
            "execution_mode": "any"
        }]
    });

    let response = send_json(
        app,
        Method::PUT,
        "/api/deployments",
        &[("x-ploy-admin-token", "admin-test-token")],
        Some(payload),
    )
    .await;

    assert_eq!(
        response.0,
        StatusCode::OK,
        "failed to upsert deployment: {}",
        response.1
    );
}

async fn send_json(
    app: &Router,
    method: Method,
    uri: &str,
    headers: &[(&str, &str)],
    body: Option<Value>,
) -> (StatusCode, String) {
    let mut request_builder = Request::builder().method(method).uri(uri);
    for (key, value) in headers {
        request_builder = request_builder.header(*key, *value);
    }

    let request = if let Some(payload) = body {
        request_builder
            .header("content-type", "application/json")
            .body(Body::from(payload.to_string()))
            .expect("failed to build json request")
    } else {
        request_builder
            .body(Body::empty())
            .expect("failed to build empty request")
    };

    let response = app
        .clone()
        .oneshot(request)
        .await
        .expect("router request failed");
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("failed to read response body");
    let body = String::from_utf8_lossy(&bytes).to_string();

    (status, body)
}

#[tokio::test]
async fn strategy_evaluations_roundtrip_with_dedupe() {
    let _guard = env_lock().lock().expect("failed to acquire env lock");
    let Some(ctx) = TestContext::new(&[]).await else {
        return;
    };

    let payload = json!({
        "strategy_id": "strategy-alpha",
        "domain": "crypto",
        "stage": "paper",
        "status": "pass",
        "score": 0.74,
        "sample_size": 128,
        "evidence_kind": "backtest",
        "evidence_ref": "s3://reports/strategy-alpha-paper.json",
        "evidence_hash": "hash-alpha-paper-1"
    });

    let (status, body) = send_json(
        &ctx.app,
        Method::POST,
        "/api/sidecar/strategy-evaluations",
        &[("x-ploy-sidecar-token", "sidecar-test-token")],
        Some(payload.clone()),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "unexpected response: {body}");
    let first: Value = serde_json::from_str(&body).expect("invalid json response");
    assert_eq!(first["deduped"], Value::Bool(false));
    let evaluation_id = first["evaluation_id"]
        .as_i64()
        .expect("missing evaluation_id");

    let (status, body) = send_json(
        &ctx.app,
        Method::POST,
        "/api/sidecar/strategy-evaluations",
        &[("x-ploy-sidecar-token", "sidecar-test-token")],
        Some(payload),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "unexpected response: {body}");
    let second: Value = serde_json::from_str(&body).expect("invalid json response");
    assert_eq!(second["deduped"], Value::Bool(true));
    assert_eq!(
        second["evaluation_id"].as_i64(),
        Some(evaluation_id),
        "dedupe should return original evaluation id"
    );

    let (status, body) = send_json(
        &ctx.app,
        Method::GET,
        "/api/sidecar/strategy-evaluations?strategy_id=strategy-alpha&stage=paper",
        &[("x-ploy-sidecar-token", "sidecar-test-token")],
        None,
    )
    .await;

    assert_eq!(status, StatusCode::OK, "unexpected response: {body}");
    let rows: Vec<Value> = serde_json::from_str(&body).expect("invalid evaluations list");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["strategy_id"], "strategy-alpha");
    assert_eq!(rows[0]["stage"], "PAPER");
    assert_eq!(rows[0]["status"], "PASS");
}

#[tokio::test]
async fn deployment_enable_rejects_missing_required_stage_evidence() {
    let _guard = env_lock().lock().expect("failed to acquire env lock");
    let Some(ctx) = TestContext::new(&[
        ("PLOY_DEPLOYMENTS_REQUIRE_EVIDENCE", "true"),
        ("PLOY_DEPLOYMENTS_REQUIRED_STAGES", "backtest,paper"),
        ("PLOY_DEPLOYMENTS_MAX_EVIDENCE_AGE_HOURS", "24"),
    ])
    .await
    else {
        return;
    };

    let deployment_id = "dep-missing-stage";
    let strategy_id = "strategy-missing-stage";

    upsert_disabled_deployment(&ctx.app, deployment_id, strategy_id).await;
    insert_evaluation(
        &ctx.pool,
        strategy_id,
        "BACKTEST",
        "PASS",
        Utc::now() - ChronoDuration::hours(1),
    )
    .await;

    let (status, body) = send_json(
        &ctx.app,
        Method::POST,
        &format!("/api/deployments/{deployment_id}/enable"),
        &[("x-ploy-admin-token", "admin-test-token")],
        None,
    )
    .await;

    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(
        body.contains("missing PAPER evidence"),
        "expected missing-stage error, got: {body}"
    );
}

#[tokio::test]
async fn deployment_enable_rejects_non_pass_evidence() {
    let _guard = env_lock().lock().expect("failed to acquire env lock");
    let Some(ctx) = TestContext::new(&[
        ("PLOY_DEPLOYMENTS_REQUIRE_EVIDENCE", "true"),
        ("PLOY_DEPLOYMENTS_REQUIRED_STAGES", "backtest,paper"),
        ("PLOY_DEPLOYMENTS_MAX_EVIDENCE_AGE_HOURS", "24"),
    ])
    .await
    else {
        return;
    };

    let deployment_id = "dep-non-pass";
    let strategy_id = "strategy-non-pass";

    upsert_disabled_deployment(&ctx.app, deployment_id, strategy_id).await;
    insert_evaluation(
        &ctx.pool,
        strategy_id,
        "BACKTEST",
        "PASS",
        Utc::now() - ChronoDuration::hours(1),
    )
    .await;
    insert_evaluation(
        &ctx.pool,
        strategy_id,
        "PAPER",
        "FAIL",
        Utc::now() - ChronoDuration::hours(1),
    )
    .await;

    let (status, body) = send_json(
        &ctx.app,
        Method::POST,
        &format!("/api/deployments/{deployment_id}/enable"),
        &[("x-ploy-admin-token", "admin-test-token")],
        None,
    )
    .await;

    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(
        body.contains("status is FAIL"),
        "expected non-pass status error, got: {body}"
    );
}

#[tokio::test]
async fn deployment_enable_rejects_stale_evidence() {
    let _guard = env_lock().lock().expect("failed to acquire env lock");
    let Some(ctx) = TestContext::new(&[
        ("PLOY_DEPLOYMENTS_REQUIRE_EVIDENCE", "true"),
        ("PLOY_DEPLOYMENTS_REQUIRED_STAGES", "backtest,paper"),
        ("PLOY_DEPLOYMENTS_MAX_EVIDENCE_AGE_HOURS", "1"),
    ])
    .await
    else {
        return;
    };

    let deployment_id = "dep-stale";
    let strategy_id = "strategy-stale";

    upsert_disabled_deployment(&ctx.app, deployment_id, strategy_id).await;
    insert_evaluation(
        &ctx.pool,
        strategy_id,
        "BACKTEST",
        "PASS",
        Utc::now() - ChronoDuration::hours(8),
    )
    .await;
    insert_evaluation(
        &ctx.pool,
        strategy_id,
        "PAPER",
        "PASS",
        Utc::now() - ChronoDuration::minutes(5),
    )
    .await;

    let (status, body) = send_json(
        &ctx.app,
        Method::POST,
        &format!("/api/deployments/{deployment_id}/enable"),
        &[("x-ploy-admin-token", "admin-test-token")],
        None,
    )
    .await;

    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(
        body.contains("stale"),
        "expected stale evidence error, got: {body}"
    );
}
