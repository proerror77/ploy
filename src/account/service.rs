use crate::config::AccountConfig;
use crate::platform::StrategyDeployment;

use super::budget::AccountBudgetSnapshot;
use super::claimer::AccountClaimerHandle;
use super::registry::AccountRegistryEntry;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountSnapshot {
    pub account_id: String,
    pub wallet_address: Option<String>,
    pub deployment_total: usize,
    pub deployment_enabled: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeAccountView {
    pub account: AccountSnapshot,
    pub budget: AccountBudgetSnapshot,
    pub claimer: AccountClaimerHandle,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountOverviewRow {
    pub account_id: String,
    pub wallet_address: Option<String>,
    pub label: Option<String>,
    pub runtime_active: bool,
    pub deployment_total: usize,
    pub deployment_enabled: usize,
}

#[derive(Debug, Clone)]
pub struct AccountService {
    registry_rows: Vec<AccountRegistryEntry>,
    deployments: Vec<StrategyDeployment>,
    budget: AccountBudgetSnapshot,
    claimer: AccountClaimerHandle,
}

impl AccountService {
    pub fn new(
        registry_rows: Vec<AccountRegistryEntry>,
        deployments: Vec<StrategyDeployment>,
        budget: AccountBudgetSnapshot,
    ) -> Self {
        let mut normalized_rows = Vec::new();
        for row in registry_rows {
            if let Some(row) = row.normalized() {
                if !normalized_rows.iter().any(|existing: &AccountRegistryEntry| {
                    existing.account_id.eq_ignore_ascii_case(&row.account_id)
                }) {
                    normalized_rows.push(row);
                }
            }
        }

        Self {
            registry_rows: normalized_rows,
            deployments,
            budget,
            claimer: AccountClaimerHandle,
        }
    }

    pub fn claimer_handle(&self) -> AccountClaimerHandle {
        self.claimer
    }

    pub fn resolve_runtime_account(&self, account_cfg: &AccountConfig) -> RuntimeAccountView {
        let configured = AccountRegistryEntry::from_account_config(account_cfg);
        let matched_row = self
            .registry_rows
            .iter()
            .find(|row| row.account_id.eq_ignore_ascii_case(&configured.account_id));

        let runtime_account = matched_row
            .map(|row| AccountRegistryEntry {
                account_id: row.account_id.clone(),
                wallet_address: row
                    .wallet_address
                    .clone()
                    .or_else(|| configured.wallet_address.clone()),
                label: row.label.clone().or_else(|| configured.label.clone()),
            })
            .unwrap_or(configured);

        let (deployment_total, deployment_enabled) =
            self.deployment_coverage(runtime_account.account_id.as_str());

        RuntimeAccountView {
            account: AccountSnapshot {
                account_id: runtime_account.account_id,
                wallet_address: runtime_account.wallet_address,
                deployment_total,
                deployment_enabled,
            },
            budget: self.budget.clone(),
            claimer: self.claimer,
        }
    }

    pub fn accounts_overview(&self, runtime_account_id: &str) -> Vec<AccountOverviewRow> {
        let runtime_account_id = AccountRegistryEntry::normalize_account_id(runtime_account_id);
        let mut rows = self.registry_rows.clone();
        if !rows
            .iter()
            .any(|row| row.account_id.eq_ignore_ascii_case(&runtime_account_id))
        {
            rows.push(AccountRegistryEntry {
                account_id: runtime_account_id.clone(),
                wallet_address: None,
                label: Some("runtime".to_string()),
            });
        }

        let mut overview = rows
            .into_iter()
            .map(|row| {
                let (deployment_total, deployment_enabled) =
                    self.deployment_coverage(row.account_id.as_str());
                AccountOverviewRow {
                    runtime_active: row.account_id.eq_ignore_ascii_case(&runtime_account_id),
                    account_id: row.account_id,
                    wallet_address: row.wallet_address,
                    label: row.label,
                    deployment_total,
                    deployment_enabled,
                }
            })
            .collect::<Vec<_>>();
        overview.sort_by(|a, b| a.account_id.cmp(&b.account_id));
        overview
    }

    fn deployment_coverage(&self, account_id: &str) -> (usize, usize) {
        let mut total = 0usize;
        let mut enabled = 0usize;
        for deployment in &self.deployments {
            if deployment.matches_account(account_id) {
                total += 1;
                if deployment.enabled {
                    enabled += 1;
                }
            }
        }
        (total, enabled)
    }
}

#[cfg(test)]
mod tests {
    use rust_decimal::Decimal;

    use crate::config::AccountConfig;
    use crate::platform::{
        DeploymentExecutionMode, Domain, MarketSelector, StrategyDeployment,
        StrategyLifecycleStage, StrategyProductType, Timeframe,
    };

    use super::super::budget::AccountBudgetSnapshot;
    use super::super::registry::AccountRegistryEntry;
    use super::AccountService;

    fn sample_deployment(
        id: &str,
        enabled: bool,
        account_ids: Vec<&str>,
        domain: Domain,
    ) -> StrategyDeployment {
        StrategyDeployment {
            id: id.to_string(),
            strategy: "momentum".to_string(),
            strategy_version: "v1".to_string(),
            domain,
            market_selector: MarketSelector::Static {
                symbol: Some("BTCUSDT".to_string()),
                series_id: None,
                market_slug: None,
            },
            timeframe: Timeframe::M5,
            enabled,
            state: crate::plugins::DeploymentState::Enabled,
            allocator_profile: "default".to_string(),
            risk_profile: "default".to_string(),
            priority: 0,
            cooldown_secs: 60,
            account_ids: account_ids.into_iter().map(str::to_string).collect(),
            execution_mode: DeploymentExecutionMode::Any,
            lifecycle_stage: StrategyLifecycleStage::Live,
            product_type: StrategyProductType::BinaryOption,
            last_evaluated_at: None,
            last_evaluation_score: None,
        }
    }

    #[test]
    fn resolves_runtime_account_from_config_and_registry_rows() {
        let service = AccountService::new(
            vec![AccountRegistryEntry {
                account_id: "tango".to_string(),
                wallet_address: Some("0xabc".to_string()),
                label: Some("Main".to_string()),
            }],
            vec![sample_deployment(
                "deploy.crypto.momentum.1",
                true,
                vec!["tango"],
                Domain::Crypto,
            )],
            AccountBudgetSnapshot {
                available_notional_usd: Decimal::new(900, 0),
                reserved_notional_usd: Decimal::new(100, 0),
            },
        );

        let runtime = service.resolve_runtime_account(&AccountConfig {
            id: "tango".to_string(),
            wallet_address: None,
            label: Some("Runtime".to_string()),
        });

        assert_eq!(runtime.account.account_id, "tango");
        assert_eq!(runtime.account.wallet_address.as_deref(), Some("0xabc"));
        assert_eq!(runtime.account.deployment_total, 1);
        assert_eq!(runtime.account.deployment_enabled, 1);
    }

    #[test]
    fn account_service_exposes_account_scoped_claimer_handle() {
        let service = AccountService::new(
            Vec::new(),
            Vec::new(),
            AccountBudgetSnapshot::default(),
        );

        let handle = service.claimer_handle();

        assert_eq!(
            std::any::type_name_of_val(&handle),
            "ploy::account::claimer::AccountClaimerHandle"
        );
    }

    #[test]
    fn runtime_account_snapshot_returns_deployment_coverage_and_budget_together() {
        let service = AccountService::new(
            vec![AccountRegistryEntry {
                account_id: "tango".to_string(),
                wallet_address: Some("0xabc".to_string()),
                label: Some("Main".to_string()),
            }],
            vec![
                sample_deployment("deploy.crypto.momentum.1", true, vec!["tango"], Domain::Crypto),
                sample_deployment(
                    "deploy.crypto.split_arb.1",
                    false,
                    vec!["tango"],
                    Domain::Crypto,
                ),
            ],
            AccountBudgetSnapshot {
                available_notional_usd: Decimal::new(850, 0),
                reserved_notional_usd: Decimal::new(150, 0),
            },
        );

        let runtime = service.resolve_runtime_account(&AccountConfig {
            id: "tango".to_string(),
            wallet_address: None,
            label: None,
        });

        assert_eq!(runtime.account.deployment_total, 2);
        assert_eq!(runtime.account.deployment_enabled, 1);
        assert_eq!(runtime.budget.available_notional_usd, Decimal::new(850, 0));
        assert_eq!(runtime.budget.reserved_notional_usd, Decimal::new(150, 0));
    }
}
