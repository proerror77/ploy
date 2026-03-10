use rust_decimal::Decimal;
use tokio::sync::RwLock;
use uuid::Uuid;

use crate::coordinator::OrderIntent;
use crate::domain::Domain;

mod crypto;
mod market;

use self::crypto::CryptoCapitalAllocator;
pub(in crate::coordinator) use self::crypto::CryptoHorizon;
use self::market::MarketCapitalAllocator;
pub(in crate::coordinator) use self::market::{
    intent_deployment_scope, intent_market_identity, sell_release_reference_price,
};
use super::command::{AllocatorLedgerSnapshot, DeploymentLedgerSnapshot};
use super::config::CoordinatorConfig;

const KNOWN_5M_SERIES_IDS: &[&str] = &["10684", "10683", "10686", "10685"];
const KNOWN_15M_SERIES_IDS: &[&str] = &["10192", "10191", "10423", "10422"];

#[derive(Debug)]
pub(super) struct CapitalPolicy {
    crypto: RwLock<CryptoCapitalAllocator>,
    sports: RwLock<MarketCapitalAllocator>,
    politics: RwLock<MarketCapitalAllocator>,
    economics: RwLock<MarketCapitalAllocator>,
}

impl CapitalPolicy {
    pub(super) fn new(config: &CoordinatorConfig) -> Self {
        Self {
            crypto: RwLock::new(CryptoCapitalAllocator::new(config)),
            sports: RwLock::new(MarketCapitalAllocator::for_sports(config)),
            politics: RwLock::new(MarketCapitalAllocator::for_politics(config)),
            economics: RwLock::new(MarketCapitalAllocator::for_economics(config)),
        }
    }

    pub(super) async fn reset_runtime_state(&self) {
        self.crypto.write().await.reset_runtime_state();
        self.sports.write().await.reset_runtime_state();
        self.politics.write().await.reset_runtime_state();
        self.economics.write().await.reset_runtime_state();
    }

    pub(super) async fn allocator_totals(&self) -> (Decimal, Decimal) {
        let (crypto_open, crypto_pending) = {
            let allocator = self.crypto.read().await;
            (allocator.open_notional(), allocator.pending_notional())
        };
        let (sports_open, sports_pending) = {
            let allocator = self.sports.read().await;
            (allocator.open_notional(), allocator.pending_notional())
        };
        let (politics_open, politics_pending) = {
            let allocator = self.politics.read().await;
            (allocator.open_notional(), allocator.pending_notional())
        };
        let (economics_open, economics_pending) = {
            let allocator = self.economics.read().await;
            (allocator.open_notional(), allocator.pending_notional())
        };

        (
            crypto_open + sports_open + politics_open + economics_open,
            crypto_pending + sports_pending + politics_pending + economics_pending,
        )
    }

    pub(super) async fn ledger_rows(
        &self,
    ) -> (
        Vec<AllocatorLedgerSnapshot>,
        Vec<DeploymentLedgerSnapshot>,
        Decimal,
        Decimal,
    ) {
        let (crypto, mut deployments) = {
            let allocator = self.crypto.read().await;
            (
                allocator.ledger_snapshot(),
                allocator.deployment_ledger_snapshot(),
            )
        };
        let (sports, sports_deployments) = {
            let allocator = self.sports.read().await;
            (
                allocator.ledger_snapshot(),
                allocator.deployment_ledger_snapshot(),
            )
        };
        deployments.extend(sports_deployments);
        let (politics, politics_deployments) = {
            let allocator = self.politics.read().await;
            (
                allocator.ledger_snapshot(),
                allocator.deployment_ledger_snapshot(),
            )
        };
        deployments.extend(politics_deployments);
        let (economics, economics_deployments) = {
            let allocator = self.economics.read().await;
            (
                allocator.ledger_snapshot(),
                allocator.deployment_ledger_snapshot(),
            )
        };
        deployments.extend(economics_deployments);
        deployments.sort_by(|a, b| {
            a.domain
                .cmp(&b.domain)
                .then_with(|| a.deployment_id.cmp(&b.deployment_id))
        });

        let allocator_open_notional = crypto.open_notional_usd
            + sports.open_notional_usd
            + politics.open_notional_usd
            + economics.open_notional_usd;
        let allocator_pending_notional = crypto.pending_notional_usd
            + sports.pending_notional_usd
            + politics.pending_notional_usd
            + economics.pending_notional_usd;

        (
            vec![crypto, sports, politics, economics],
            deployments,
            allocator_open_notional,
            allocator_pending_notional,
        )
    }

    pub(super) async fn available_notional_for(&self, intent: &OrderIntent) -> Option<Decimal> {
        match intent.domain {
            Domain::Crypto => self.crypto.read().await.available_notional_for(intent),
            Domain::Sports => self.sports.read().await.available_notional_for(intent),
            _ => None,
        }
    }

    pub(super) async fn reserve_buy(&self, intent: &OrderIntent) -> Option<String> {
        if !intent.is_buy {
            return None;
        }
        match intent.domain {
            Domain::Crypto => self.crypto.write().await.reserve_buy(intent).err(),
            Domain::Sports => self.sports.write().await.reserve_buy(intent).err(),
            Domain::Politics => self.politics.write().await.reserve_buy(intent).err(),
            Domain::Economics => self.economics.write().await.reserve_buy(intent).err(),
            _ => None,
        }
    }

    pub(super) async fn release_buy_reservation(&self, intent_id: Uuid) {
        self.crypto.write().await.release_buy_reservation(intent_id);
        self.sports.write().await.release_buy_reservation(intent_id);
        self.politics
            .write()
            .await
            .release_buy_reservation(intent_id);
        self.economics
            .write()
            .await
            .release_buy_reservation(intent_id);
    }

    pub(super) async fn settle_success(
        &self,
        intent: &OrderIntent,
        filled_shares: u64,
        fill_price: Decimal,
    ) {
        match intent.domain {
            Domain::Crypto => {
                let mut allocator = self.crypto.write().await;
                if intent.is_buy {
                    allocator.settle_buy_execution(intent, filled_shares, fill_price);
                } else {
                    allocator.settle_sell_execution(intent, filled_shares, fill_price);
                }
            }
            Domain::Sports => {
                let mut allocator = self.sports.write().await;
                if intent.is_buy {
                    allocator.settle_buy_execution(intent, filled_shares, fill_price);
                } else {
                    allocator.settle_sell_execution(intent, filled_shares, fill_price);
                }
            }
            Domain::Politics => {
                let mut allocator = self.politics.write().await;
                if intent.is_buy {
                    allocator.settle_buy_execution(intent, filled_shares, fill_price);
                } else {
                    allocator.settle_sell_execution(intent, filled_shares, fill_price);
                }
            }
            Domain::Economics => {
                let mut allocator = self.economics.write().await;
                if intent.is_buy {
                    allocator.settle_buy_execution(intent, filled_shares, fill_price);
                } else {
                    allocator.settle_sell_execution(intent, filled_shares, fill_price);
                }
            }
            _ => {}
        }
    }

    pub(super) async fn settle_failure(&self, intent: &OrderIntent) {
        if !intent.is_buy {
            return;
        }
        match intent.domain {
            Domain::Crypto => self
                .crypto
                .write()
                .await
                .release_buy_reservation(intent.intent_id),
            Domain::Sports => self
                .sports
                .write()
                .await
                .release_buy_reservation(intent.intent_id),
            Domain::Politics => self
                .politics
                .write()
                .await
                .release_buy_reservation(intent.intent_id),
            Domain::Economics => self
                .economics
                .write()
                .await
                .release_buy_reservation(intent.intent_id),
            _ => {}
        }
    }
}
