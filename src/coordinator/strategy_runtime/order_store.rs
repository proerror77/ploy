use async_trait::async_trait;
use rust_decimal::Decimal;
use tracing::warn;

use crate::adapters::PostgresStore;
use crate::domain::{OrderRequest, OrderStatus};
use crate::error::Result;

#[async_trait]
pub(crate) trait RuntimeOrderStore: Send + Sync {
    async fn insert_order(&self, order: &crate::domain::Order) -> Result<()>;
    async fn update_order_status(
        &self,
        client_order_id: &str,
        status: OrderStatus,
        exchange_order_id: Option<&str>,
    ) -> Result<()>;
    async fn update_order_fill(
        &self,
        client_order_id: &str,
        filled_shares: u64,
        avg_fill_price: Decimal,
        status: OrderStatus,
    ) -> Result<()>;
}

#[async_trait]
impl RuntimeOrderStore for PostgresStore {
    async fn insert_order(&self, order: &crate::domain::Order) -> Result<()> {
        PostgresStore::insert_order(self, order).await.map(|_| ())
    }

    async fn update_order_status(
        &self,
        client_order_id: &str,
        status: OrderStatus,
        exchange_order_id: Option<&str>,
    ) -> Result<()> {
        PostgresStore::update_order_status(self, client_order_id, status, exchange_order_id).await
    }

    async fn update_order_fill(
        &self,
        client_order_id: &str,
        filled_shares: u64,
        avg_fill_price: Decimal,
        status: OrderStatus,
    ) -> Result<()> {
        PostgresStore::update_order_fill(
            self,
            client_order_id,
            filled_shares,
            avg_fill_price,
            status,
        )
        .await
    }
}

pub(crate) fn normalize_runtime_order_request(client_order_id: &str, order: &mut OrderRequest) {
    if order.client_order_id != client_order_id {
        warn!(
            "Mismatched order IDs in managed runtime action: action={}, request={}; using action ID",
            client_order_id, order.client_order_id
        );
        order.client_order_id = client_order_id.to_string();
    }
    let missing_idempotency = order
        .idempotency_key
        .as_deref()
        .map(str::trim)
        .is_none_or(str::is_empty);
    if missing_idempotency {
        order.idempotency_key = Some(client_order_id.to_string());
    }
}

fn runtime_order_leg(client_order_id: &str) -> u8 {
    if client_order_id.contains("leg2") {
        2
    } else {
        1
    }
}

pub(crate) async fn persist_runtime_order_insert(
    store: &dyn RuntimeOrderStore,
    strategy_id: &str,
    order: &OrderRequest,
) -> Result<()> {
    let db_order = crate::domain::Order::from_request(
        order,
        None,
        runtime_order_leg(&order.client_order_id),
        Some(strategy_id.to_string()),
    );
    store.insert_order(&db_order).await
}

pub(crate) async fn persist_runtime_order_result(
    store: &dyn RuntimeOrderStore,
    client_order_id: &str,
    exchange_order_id: &str,
    status: OrderStatus,
    filled_shares: u64,
    avg_fill_price: Option<Decimal>,
    fallback_price: Decimal,
) -> Result<()> {
    store
        .update_order_status(
            client_order_id,
            OrderStatus::Submitted,
            Some(exchange_order_id),
        )
        .await?;

    if filled_shares > 0 {
        store
            .update_order_fill(
                client_order_id,
                filled_shares,
                avg_fill_price.unwrap_or(fallback_price),
                status,
            )
            .await?;
    } else if status != OrderStatus::Submitted {
        store
            .update_order_status(client_order_id, status, Some(exchange_order_id))
            .await?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        normalize_runtime_order_request, persist_runtime_order_insert,
        persist_runtime_order_result, RuntimeOrderStore,
    };
    use crate::domain::{OrderRequest, OrderStatus, Side};
    use crate::error::Result;
    use async_trait::async_trait;
    use rust_decimal_macros::dec;
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct MockRuntimeOrderStore {
        inserted: Mutex<Vec<crate::domain::Order>>,
        status_updates: Mutex<Vec<(String, OrderStatus, Option<String>)>>,
        fill_updates: Mutex<Vec<(String, u64, rust_decimal::Decimal, OrderStatus)>>,
    }

    #[async_trait]
    impl RuntimeOrderStore for MockRuntimeOrderStore {
        async fn insert_order(&self, order: &crate::domain::Order) -> Result<()> {
            self.inserted
                .lock()
                .expect("inserted lock")
                .push(order.clone());
            Ok(())
        }

        async fn update_order_status(
            &self,
            client_order_id: &str,
            status: OrderStatus,
            exchange_order_id: Option<&str>,
        ) -> Result<()> {
            self.status_updates
                .lock()
                .expect("status_updates lock")
                .push((
                    client_order_id.to_string(),
                    status,
                    exchange_order_id.map(str::to_string),
                ));
            Ok(())
        }

        async fn update_order_fill(
            &self,
            client_order_id: &str,
            filled_shares: u64,
            avg_fill_price: rust_decimal::Decimal,
            status: OrderStatus,
        ) -> Result<()> {
            self.fill_updates.lock().expect("fill_updates lock").push((
                client_order_id.to_string(),
                filled_shares,
                avg_fill_price,
                status,
            ));
            Ok(())
        }
    }

    #[tokio::test]
    async fn persist_runtime_order_insert_uses_action_order_id_and_leg() {
        let store = Arc::new(MockRuntimeOrderStore::default());
        let mut order = OrderRequest::buy_limit("token-1".to_string(), Side::Down, 20, dec!(0.55));

        normalize_runtime_order_request("stag_leg2_merge_123", &mut order);
        persist_runtime_order_insert(store.as_ref(), "staggered_arb_strategy", &order)
            .await
            .expect("insert should succeed");

        let inserted = store.inserted.lock().expect("inserted lock");
        assert_eq!(inserted.len(), 1);
        assert_eq!(inserted[0].client_order_id, "stag_leg2_merge_123");
        assert_eq!(inserted[0].leg, 2);
        assert_eq!(
            inserted[0].strategy_id.as_deref(),
            Some("staggered_arb_strategy")
        );
    }

    #[test]
    fn normalize_runtime_order_request_sets_idempotency_key_from_action_id() {
        let mut order = OrderRequest::buy_limit("token-1".to_string(), Side::Up, 20, dec!(0.40));
        order.client_order_id = "mismatched".to_string();
        order.idempotency_key = None;

        normalize_runtime_order_request("stag_leg1_123", &mut order);

        assert_eq!(order.client_order_id, "stag_leg1_123");
        assert_eq!(order.idempotency_key.as_deref(), Some("stag_leg1_123"));
    }

    #[tokio::test]
    async fn persist_runtime_order_result_records_submission_and_fill() {
        let store = Arc::new(MockRuntimeOrderStore::default());

        persist_runtime_order_result(
            store.as_ref(),
            "stag_leg1_123",
            "exchange-123",
            OrderStatus::Filled,
            20,
            Some(dec!(0.34)),
            dec!(0.40),
        )
        .await
        .expect("persist result should succeed");

        let status_updates = store.status_updates.lock().expect("status_updates lock");
        assert_eq!(status_updates.len(), 1);
        assert_eq!(
            status_updates[0],
            (
                "stag_leg1_123".to_string(),
                OrderStatus::Submitted,
                Some("exchange-123".to_string())
            )
        );
        drop(status_updates);

        let fill_updates = store.fill_updates.lock().expect("fill_updates lock");
        assert_eq!(fill_updates.len(), 1);
        assert_eq!(
            fill_updates[0],
            (
                "stag_leg1_123".to_string(),
                20,
                dec!(0.34),
                OrderStatus::Filled
            )
        );
    }
}
