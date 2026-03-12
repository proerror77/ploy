use chrono::{Duration as ChronoDuration, Utc};
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use std::collections::{HashMap, HashSet};
use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::control_plane::StrategyDeployment;
use crate::domain::{OrderRequest, OrderSide};
use crate::platform::{Domain, OrderIntent, OrderPriority};

mod deployments;
mod duplicate_guard;

use super::capital::{intent_deployment_scope, intent_market_identity, CapitalPolicy};
use super::config::{CoordinatorConfig, DuplicateGuardScope};
pub(in crate::coordinator) use duplicate_guard::IntentDuplicateGuard;

pub(super) struct AdmissionController {
    config: CoordinatorConfig,
    duplicate_guard: RwLock<IntentDuplicateGuard>,
    deployments: Arc<RwLock<HashMap<String, StrategyDeployment>>>,
}

impl AdmissionController {
    pub(super) fn new(config: &CoordinatorConfig) -> Self {
        Self {
            config: config.clone(),
            duplicate_guard: RwLock::new(IntentDuplicateGuard::new(
                config.duplicate_guard_window_ms,
                config.duplicate_guard_enabled,
                config.duplicate_guard_scope,
            )),
            deployments: Arc::new(RwLock::new(deployments::load_strategy_deployments())),
        }
    }

    pub(super) fn shared_deployments(&self) -> Arc<RwLock<HashMap<String, StrategyDeployment>>> {
        self.deployments.clone()
    }

    pub(super) async fn enforce_live_buy_deployment_gate(
        &self,
        account_id: &str,
        dry_run: bool,
        allowed_domains: &HashSet<Domain>,
        intent: &mut OrderIntent,
    ) -> std::result::Result<(), String> {
        if !intent.is_buy || dry_run || !deployments::deployment_gate_required() {
            return Ok(());
        }
        if !allowed_domains.contains(&intent.domain) {
            return Err(format!(
                "domain {} is not enabled for this runtime",
                intent.domain
            ));
        }

        let explicit_id = deployments::metadata_value(&intent.metadata, &["deployment_id"])
            .map(ToString::to_string);
        let should_refresh = {
            let deployments = self.deployments.read().await;
            deployments.is_empty()
                || explicit_id
                    .as_ref()
                    .is_some_and(|id| !deployments.contains_key(id.as_str()))
        };
        if should_refresh {
            self.refresh_strategy_deployments().await;
        }

        let deployments = self.deployments.read().await;
        deployments::enforce_deployment_gate_with_snapshot(
            account_id,
            dry_run,
            &deployments,
            intent,
        )
    }

    pub(super) async fn apply_kelly_sizing(
        &self,
        capital_policy: &CapitalPolicy,
        intent: &mut OrderIntent,
    ) -> Option<String> {
        if !self.config.kelly_sizing_enabled {
            return None;
        }
        if !intent.is_buy {
            return None;
        }
        if intent.priority == OrderPriority::Critical {
            return None;
        }
        if intent.limit_price <= Decimal::ZERO || intent.limit_price >= Decimal::ONE {
            return None;
        }

        let p = intent
            .metadata
            .get("signal_fair_value")
            .or_else(|| intent.metadata.get("signal_win_prob"))
            .and_then(|v| Decimal::from_str(v).ok())?;
        let p = p.max(Decimal::ZERO).min(Decimal::ONE);
        let price = intent.limit_price;
        let edge = p - price;

        if edge < self.config.kelly_min_edge {
            return Some(format!(
                "kelly edge {} below min {}",
                edge, self.config.kelly_min_edge
            ));
        }

        let denom = Decimal::ONE - price;
        if denom <= Decimal::ZERO {
            return Some("kelly denom <= 0".to_string());
        }

        let raw_kelly = ((p - price) / denom).max(Decimal::ZERO).min(Decimal::ONE);
        if raw_kelly <= Decimal::ZERO {
            return Some("kelly fraction <= 0 (no positive edge)".to_string());
        }

        let mut effective_fraction = (raw_kelly * self.config.kelly_fraction_multiplier)
            .max(Decimal::ZERO)
            .min(Decimal::ONE);
        if let Some(conf) = intent
            .metadata
            .get("signal_confidence")
            .and_then(|v| Decimal::from_str(v).ok())
        {
            effective_fraction *= conf.max(Decimal::ZERO).min(Decimal::ONE);
        }

        if effective_fraction <= Decimal::ZERO {
            return Some("kelly effective fraction <= 0".to_string());
        }

        let bankroll = capital_policy
            .available_notional_for(intent)
            .await
            .unwrap_or_else(|| intent.notional_value());

        if bankroll <= Decimal::ZERO {
            return Some("kelly bankroll <= 0".to_string());
        }

        let target_notional = (bankroll * effective_fraction).max(Decimal::ZERO);
        if target_notional <= Decimal::ZERO {
            return Some("kelly target_notional <= 0".to_string());
        }

        let sized_shares = (target_notional / price)
            .floor()
            .to_u64()
            .unwrap_or(0)
            .min(intent.shares);

        let mut final_shares = sized_shares;
        if final_shares == 0 {
            let floor_shares = self.config.kelly_min_shares.min(intent.shares);
            if floor_shares > 0 {
                final_shares = floor_shares;
                intent
                    .metadata
                    .insert("kelly_min_shares_applied".to_string(), "true".to_string());
                intent.metadata.insert(
                    "kelly_min_shares_floor".to_string(),
                    floor_shares.to_string(),
                );
            } else {
                return Some("kelly sizing produced 0 shares".to_string());
            }
        }

        if final_shares < intent.shares {
            intent.shares = final_shares;
        }

        intent
            .metadata
            .insert("kelly_fraction_raw".to_string(), raw_kelly.to_string());
        intent.metadata.insert(
            "kelly_fraction_multiplier".to_string(),
            self.config.kelly_fraction_multiplier.to_string(),
        );
        intent.metadata.insert(
            "kelly_fraction_effective".to_string(),
            effective_fraction.to_string(),
        );
        intent
            .metadata
            .insert("kelly_bankroll_usd".to_string(), bankroll.to_string());
        intent.metadata.insert(
            "kelly_target_notional_usd".to_string(),
            target_notional.to_string(),
        );
        intent
            .metadata
            .insert("kelly_sized_shares".to_string(), sized_shares.to_string());
        if final_shares != sized_shares {
            intent
                .metadata
                .insert("kelly_final_shares".to_string(), final_shares.to_string());
        }

        None
    }

    pub(super) fn apply_min_order_constraints(
        &self,
        intent: &mut OrderIntent,
        strategy_max_shares: u64,
    ) -> Option<String> {
        if !intent.is_buy {
            return None;
        }
        if intent.priority == OrderPriority::Critical {
            return None;
        }
        if intent.limit_price <= Decimal::ZERO {
            return None;
        }

        let min_shares_cfg = self.config.min_order_shares.max(1);
        let min_notional = self.config.min_order_notional_usd.max(Decimal::ZERO);

        let mut required_shares = min_shares_cfg;
        if min_notional > Decimal::ZERO {
            let min_shares_for_notional = (min_notional / intent.limit_price)
                .ceil()
                .to_u64()
                .unwrap_or(u64::MAX);
            required_shares = required_shares.max(min_shares_for_notional);
        }

        if required_shares <= 1 {
            return None;
        }

        if required_shares > strategy_max_shares {
            return Some(format!(
                "venue minimum requires {} shares (min_shares={}, min_notional_usd={}) but strategy_max_shares={}",
                required_shares, min_shares_cfg, min_notional, strategy_max_shares
            ));
        }

        if intent.shares < required_shares {
            let before = intent.shares;
            intent.shares = required_shares;
            intent
                .metadata
                .insert("venue_min_order_applied".to_string(), "true".to_string());
            intent.metadata.insert(
                "venue_min_order_before_shares".to_string(),
                before.to_string(),
            );
            intent.metadata.insert(
                "venue_min_order_required_shares".to_string(),
                required_shares.to_string(),
            );
            intent.metadata.insert(
                "venue_min_order_min_shares".to_string(),
                min_shares_cfg.to_string(),
            );
            intent.metadata.insert(
                "venue_min_order_min_notional_usd".to_string(),
                min_notional.to_string(),
            );
        }

        None
    }

    fn refresh_strategy_deployments(&self) -> impl std::future::Future<Output = ()> + '_ {
        async move {
            let loaded = deployments::load_strategy_deployments();
            let mut deployments = self.deployments.write().await;
            *deployments = loaded;
        }
    }
}

pub(in crate::coordinator) fn buy_intent_missing_deployment_reason(
    intent: &OrderIntent,
) -> Option<String> {
    deployments::buy_intent_missing_deployment_reason(intent)
}

pub(super) fn sell_reduce_only_violation_reason(
    intent: &OrderIntent,
    tracked_open_shares: u64,
    pending_sell_shares: u64,
) -> Option<String> {
    if intent.is_buy {
        return None;
    }

    if tracked_open_shares == 0 {
        return Some(format!(
            "SELL intent reduce-only violation: no tracked open shares for token_id={} side={} in domain={}",
            intent.token_id,
            intent.side.as_str(),
            intent.domain
        ));
    }

    let available_shares = tracked_open_shares.saturating_sub(pending_sell_shares);
    if available_shares == 0 {
        return Some(format!(
            "SELL intent reduce-only violation: tracked open shares {} are fully reserved by pending SELL intents {} for token_id={} side={}",
            tracked_open_shares,
            pending_sell_shares,
            intent.token_id,
            intent.side.as_str()
        ));
    }

    if intent.shares > available_shares {
        return Some(format!(
            "SELL intent reduce-only violation: requested shares {} exceeds available reduce-only shares {} (tracked={}, pending_sell={}) for token_id={} side={}",
            intent.shares,
            available_shares,
            tracked_open_shares,
            pending_sell_shares,
            intent.token_id,
            intent.side.as_str()
        ));
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control_plane::{
        DeploymentExecutionMode, MarketSelector, StrategyLifecycleStage, StrategyProductType,
        Timeframe,
    };
    use rust_decimal_macros::dec;

    fn make_intent(is_buy: bool, priority: OrderPriority) -> OrderIntent {
        let mut intent = OrderIntent::new(
            "crypto_lob_ml",
            Domain::Crypto,
            "btc-updown-5m-123",
            "token-up-123",
            crate::domain::Side::Up,
            is_buy,
            100,
            dec!(0.42),
        );
        intent.priority = priority;
        intent
    }

    fn make_deployment(
        id: &str,
        strategy: &str,
        domain: Domain,
        timeframe: Timeframe,
        execution_mode: DeploymentExecutionMode,
    ) -> StrategyDeployment {
        StrategyDeployment {
            id: id.to_string(),
            strategy: strategy.to_string(),
            strategy_version: "test".to_string(),
            domain,
            market_selector: MarketSelector::Dynamic {
                domain,
                query: None,
                min_liquidity_usd: None,
                max_spread_bps: None,
                min_time_remaining_secs: None,
                max_time_remaining_secs: None,
            },
            timeframe,
            enabled: true,
            allocator_profile: "default".to_string(),
            risk_profile: "default".to_string(),
            priority: 50,
            cooldown_secs: 60,
            account_ids: Vec::new(),
            execution_mode,
            lifecycle_stage: StrategyLifecycleStage::Live,
            product_type: StrategyProductType::BinaryOption,
            last_evaluated_at: None,
            last_evaluation_score: None,
        }
    }

    #[test]
    fn test_buy_intent_requires_deployment_id_metadata() {
        let intent = make_intent(true, OrderPriority::Normal);
        let reason = buy_intent_missing_deployment_reason(&intent);
        assert_eq!(
            reason.as_deref(),
            Some("BUY intent missing required metadata field 'deployment_id'")
        );
    }

    #[test]
    fn test_sell_intent_does_not_require_deployment_id_metadata() {
        let intent = make_intent(false, OrderPriority::Normal);
        assert!(buy_intent_missing_deployment_reason(&intent).is_none());
    }

    #[test]
    fn test_deployment_gate_blocks_live_buy_without_strategy_metadata() {
        let mut deployments = HashMap::new();
        deployments.insert(
            "crypto-momentum-15m".to_string(),
            make_deployment(
                "crypto-momentum-15m",
                "momentum",
                Domain::Crypto,
                Timeframe::M15,
                DeploymentExecutionMode::LiveOnly,
            ),
        );

        let mut intent = make_intent(true, OrderPriority::Normal);
        let result = deployments::enforce_deployment_gate_with_snapshot(
            "acct-a",
            false,
            &deployments,
            &mut intent,
        );

        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .contains("strategy metadata is required"));
    }

    #[test]
    fn test_deployment_gate_accepts_explicit_deployment_and_applies_metadata() {
        let mut deployments = HashMap::new();
        deployments.insert(
            "crypto-momentum-15m".to_string(),
            make_deployment(
                "crypto-momentum-15m",
                "momentum",
                Domain::Crypto,
                Timeframe::M15,
                DeploymentExecutionMode::LiveOnly,
            ),
        );

        let mut intent = make_intent(true, OrderPriority::Normal)
            .with_metadata("strategy", "crypto_momentum")
            .with_metadata("deployment_id", "crypto-momentum-15m");
        intent.market_slug = "btc-updown-15m-xyz".to_string();

        let result = deployments::enforce_deployment_gate_with_snapshot(
            "acct-a",
            false,
            &deployments,
            &mut intent,
        );

        assert!(result.is_ok());
        assert_eq!(
            intent.metadata.get("deployment_id").map(String::as_str),
            Some("crypto-momentum-15m")
        );
        assert_eq!(
            intent.metadata.get("timeframe").map(String::as_str),
            Some("15m")
        );
    }

    #[test]
    fn test_deployment_gate_blocks_ambiguous_inferred_deployments() {
        let mut deployments = HashMap::new();
        deployments.insert(
            "crypto-momentum-a".to_string(),
            make_deployment(
                "crypto-momentum-a",
                "momentum",
                Domain::Crypto,
                Timeframe::Other("other".to_string()),
                DeploymentExecutionMode::Any,
            ),
        );
        deployments.insert(
            "crypto-momentum-b".to_string(),
            make_deployment(
                "crypto-momentum-b",
                "momentum",
                Domain::Crypto,
                Timeframe::Other("other".to_string()),
                DeploymentExecutionMode::Any,
            ),
        );

        let mut intent =
            make_intent(true, OrderPriority::Normal).with_metadata("strategy", "momentum");
        intent.market_slug = "btc-updown-unknown".to_string();

        let result = deployments::enforce_deployment_gate_with_snapshot(
            "acct-a",
            false,
            &deployments,
            &mut intent,
        );

        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .contains("ambiguous deployment resolution"));
    }

    #[test]
    fn test_deployment_gate_blocks_runtime_scope_mismatch() {
        let mut deployment = make_deployment(
            "crypto-momentum-15m",
            "momentum",
            Domain::Crypto,
            Timeframe::M15,
            DeploymentExecutionMode::DryRunOnly,
        );
        deployment.account_ids = vec!["acct-b".to_string()];

        let mut deployments = HashMap::new();
        deployments.insert("crypto-momentum-15m".to_string(), deployment);

        let mut intent = make_intent(true, OrderPriority::Normal)
            .with_metadata("strategy", "momentum")
            .with_metadata("deployment_id", "crypto-momentum-15m");
        intent.market_slug = "btc-updown-15m-xyz".to_string();

        let result = deployments::enforce_deployment_gate_with_snapshot(
            "acct-a",
            false,
            &deployments,
            &mut intent,
        );

        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not eligible"));
    }

    #[test]
    fn test_deployment_gate_infers_unique_by_timeframe_hint() {
        let mut deployments = HashMap::new();
        deployments.insert(
            "crypto-momentum-5m".to_string(),
            make_deployment(
                "crypto-momentum-5m",
                "momentum",
                Domain::Crypto,
                Timeframe::M5,
                DeploymentExecutionMode::Any,
            ),
        );
        deployments.insert(
            "crypto-momentum-15m".to_string(),
            make_deployment(
                "crypto-momentum-15m",
                "momentum",
                Domain::Crypto,
                Timeframe::M15,
                DeploymentExecutionMode::Any,
            ),
        );

        let mut intent = make_intent(true, OrderPriority::Normal)
            .with_metadata("strategy", "crypto_momentum")
            .with_metadata("horizon", "15m");
        intent.market_slug = "btc-updown-15m-xyz".to_string();

        let result = deployments::enforce_deployment_gate_with_snapshot(
            "acct-a",
            false,
            &deployments,
            &mut intent,
        );

        assert!(result.is_ok());
        assert_eq!(
            intent.metadata.get("deployment_id").map(String::as_str),
            Some("crypto-momentum-15m")
        );
    }
}
