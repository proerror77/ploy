use super::*;

impl PolymarketClient {
    /// Get market by condition ID
    #[instrument(skip(self))]
    pub async fn get_market(&self, condition_id: &str) -> Result<MarketResponse> {
        // Gamma's `market_by_id` is keyed by Gamma market id, not `condition_id`.
        // Use `markets?condition_ids=...` to fetch by condition id.
        let cond_b256 = condition_id.parse::<B256>().map_err(|e| {
            PloyError::Internal(format!("Invalid condition_id '{}': {}", condition_id, e))
        })?;

        let req = MarketsRequest::builder()
            .condition_ids(vec![cond_b256])
            .limit(1)
            .build();

        let markets = self
            .gamma_client
            .markets(&req)
            .await
            .map_err(|e| PloyError::Internal(format!("Failed to get market: {}", e)))?;

        let market = markets.into_iter().next().ok_or_else(|| {
            PloyError::MarketDataUnavailable(format!(
                "Market not found for condition_id={}",
                condition_id
            ))
        })?;

        let token_ids: Vec<String> = market
            .clob_token_ids
            .as_ref()
            .map(|ids| ids.iter().map(|id| id.to_string()).collect())
            .unwrap_or_default();
        let outcomes: Vec<String> = market
            .outcomes
            .as_ref()
            .map(|o| o.clone())
            .unwrap_or_default();
        let prices: Vec<String> = market
            .outcome_prices
            .as_ref()
            .map(|ps| ps.iter().map(|d| d.to_string()).collect())
            .unwrap_or_default();

        let mut tokens = Vec::new();
        for (i, token_id) in token_ids.iter().enumerate() {
            let outcome = outcomes.get(i).cloned().unwrap_or_default();
            let price = prices.get(i).cloned();
            tokens.push(TokenInfo {
                token_id: token_id.clone(),
                outcome,
                price,
                extra: HashMap::new(),
            });
        }

        Ok(MarketResponse {
            condition_id: market
                .condition_id
                .map(|b| b.to_string())
                .unwrap_or_else(|| condition_id.to_string()),
            question_id: market.question_id.map(|b| b.to_string()),
            tokens,
            minimum_order_size: market
                .order_min_size
                .as_ref()
                .map(|d| serde_json::json!(d.to_string())),
            minimum_tick_size: market
                .order_price_min_tick_size
                .as_ref()
                .map(|d| serde_json::json!(d.to_string())),
            active: market.active.unwrap_or(true),
            closed: market.closed.unwrap_or(false),
            end_date_iso: market
                .end_date_iso
                .map(|d| d.to_string())
                .or_else(|| market.end_date.map(|d| d.to_rfc3339())),
            neg_risk: None,
            extra: HashMap::new(),
        })
    }

    /// Get order book for a token
    #[instrument(skip(self))]
    pub async fn get_order_book(&self, token_id: &str) -> Result<OrderBookResponse> {
        let token_u256 = U256::from_str(token_id)
            .map_err(|e| PloyError::Internal(format!("Invalid token_id '{}': {}", token_id, e)))?;

        let req = OrderBookSummaryRequest::builder()
            .token_id(token_u256)
            .build();

        let resp = self
            .clob_client
            .order_book(&req)
            .await
            .map_err(|e| PloyError::Internal(format!("Failed to get order book: {}", e)))?;

        Ok(OrderBookResponse {
            market: Some(resp.market.to_string()),
            asset_id: resp.asset_id.to_string(),
            bids: resp
                .bids
                .into_iter()
                .map(|l| OrderBookLevel {
                    price: l.price.to_string(),
                    size: l.size.to_string(),
                })
                .collect(),
            asks: resp
                .asks
                .into_iter()
                .map(|l| OrderBookLevel {
                    price: l.price.to_string(),
                    size: l.size.to_string(),
                })
                .collect(),
            timestamp: Some(resp.timestamp.to_rfc3339()),
            hash: resp.hash,
        })
    }

    /// Get best bid/ask prices
    #[instrument(skip(self))]
    pub async fn get_best_prices(
        &self,
        token_id: &str,
    ) -> Result<(Option<Decimal>, Option<Decimal>)> {
        let order_book = self.get_order_book(token_id).await?;

        let best_bid = order_book
            .bids
            .first()
            .and_then(|l| l.price.parse::<Decimal>().ok());
        let best_ask = order_book
            .asks
            .first()
            .and_then(|l| l.price.parse::<Decimal>().ok());

        Ok((best_bid, best_ask))
    }

    // ==================== Trading Methods ====================

    /// Submit an order
    #[instrument(skip(self))]
    pub async fn submit_order(&self, request: &OrderRequest) -> Result<OrderResponse> {
        Self::validate_gateway_execution_context(self.dry_run)?;
        if !self.dry_run {
            Self::validate_gateway_order_request(request)?;
        }

        if self.dry_run {
            info!(
                "DRY RUN: Would submit {} order for {} shares of {} @ {}",
                request.order_side, request.shares, request.token_id, request.limit_price
            );

            let sdk_order_type = match request.time_in_force {
                TimeInForce::GTC => SdkOrderType::GTC,
                TimeInForce::FOK => SdkOrderType::FOK,
                // Polymarket SDK uses FAK (Fill and Kill) for IOC semantics.
                TimeInForce::IOC => SdkOrderType::FAK,
            };

            return Ok(OrderResponse {
                id: request.client_order_id.clone(),
                status: "OPEN".to_string(),
                owner: None,
                market: None,
                asset_id: Some(request.token_id.clone()),
                side: Some(format!("{:?}", request.order_side)),
                original_size: Some(request.shares.to_string()),
                size_matched: Some("0".to_string()),
                price: Some(request.limit_price.to_string()),
                associate_trades: None,
                created_at: Some(Utc::now().to_rfc3339()),
                expiration: None,
                order_type: Some(sdk_order_type.to_string()),
            });
        }

        let signer = self
            .signer
            .as_ref()
            .ok_or_else(|| PloyError::Auth("Not authenticated".to_string()))?;

        // Serialize order submit + auth handshake to avoid repeatedly creating API keys.
        let _guard = self.order_mutex.lock().await;
        let auth_client = self.authenticate_cached(signer).await?;

        // Build the order
        let sdk_side = match request.order_side {
            OrderSide::Buy => SdkSide::Buy,
            OrderSide::Sell => SdkSide::Sell,
        };

        let sdk_order_type = match request.time_in_force {
            TimeInForce::GTC => SdkOrderType::GTC,
            TimeInForce::FOK => SdkOrderType::FOK,
            // Polymarket SDK uses FAK (Fill and Kill) for IOC semantics.
            TimeInForce::IOC => SdkOrderType::FAK,
        };

        let token_u256 = U256::from_str(&request.token_id).map_err(|e| {
            PloyError::OrderSubmission(format!("Invalid token_id '{}': {}", request.token_id, e))
        })?;

        let order = auth_client
            .limit_order()
            .token_id(token_u256)
            .price(request.limit_price)
            .size(Decimal::from(request.shares))
            .side(sdk_side)
            .order_type(sdk_order_type)
            .build()
            .await
            .map_err(|e| PloyError::OrderSubmission(format!("Failed to build order: {}", e)))?;

        // Sign and submit
        let signed = auth_client
            .sign(signer, order)
            .await
            .map_err(|e| PloyError::OrderSubmission(format!("Failed to sign order: {}", e)))?;

        let resp = auth_client
            .post_order(signed)
            .await
            .map_err(|e| PloyError::OrderSubmission(format!("Failed to post order: {}", e)))?;

        info!("Order submitted successfully: {:?}", resp);

        Ok(OrderResponse {
            id: resp.order_id,
            status: format!("{:?}", resp.status),
            owner: None,
            market: None,
            asset_id: Some(request.token_id.clone()),
            side: Some(format!("{:?}", request.order_side)),
            original_size: Some(request.shares.to_string()),
            // Preserve immediate fill information from submit response.
            // This lets executor/strategy correctly classify synchronous matches.
            size_matched: Some(resp.taking_amount.to_string()),
            price: Some(request.limit_price.to_string()),
            associate_trades: None,
            created_at: Some(Utc::now().to_rfc3339()),
            expiration: None,
            order_type: Some(format!("{:?}", request.time_in_force)),
        })
    }

    /// Get order by ID
    #[instrument(skip(self))]
    pub async fn get_order(&self, order_id: &str) -> Result<OrderResponse> {
        let signer = self
            .signer
            .as_ref()
            .ok_or_else(|| PloyError::Auth("Not authenticated".to_string()))?;

        let _guard = self.order_mutex.lock().await;
        let auth_client = self.authenticate_cached(signer).await?;

        let order = auth_client
            .order(order_id)
            .await
            .map_err(|e| PloyError::Internal(format!("Failed to get order: {}", e)))?;

        Ok(OrderResponse {
            id: order.id,
            status: format!("{:?}", order.status),
            owner: Some(order.owner.to_string()),
            market: Some(order.market.to_string()),
            asset_id: Some(order.asset_id.to_string()),
            side: Some(format!("{:?}", order.side)),
            original_size: Some(order.original_size.to_string()),
            size_matched: Some(order.size_matched.to_string()),
            price: Some(order.price.to_string()),
            associate_trades: None,
            created_at: Some(order.created_at.to_rfc3339()),
            expiration: Some(order.expiration.to_rfc3339()),
            order_type: Some(format!("{:?}", order.order_type)),
        })
    }

    /// Cancel an order
    #[instrument(skip(self))]
    pub async fn cancel_order(&self, order_id: &str) -> Result<bool> {
        if self.dry_run {
            info!("DRY RUN: Would cancel order {}", order_id);
            return Ok(true);
        }

        let signer = self
            .signer
            .as_ref()
            .ok_or_else(|| PloyError::Auth("Not authenticated".to_string()))?;

        let _guard = self.order_mutex.lock().await;
        let auth_client = self.authenticate_cached(signer).await?;

        auth_client
            .cancel_order(order_id)
            .await
            .map_err(|e| PloyError::Internal(format!("Failed to cancel order: {}", e)))?;

        Ok(true)
    }

    /// Cancel all orders for a token
    #[instrument(skip(self))]
    pub async fn cancel_all_orders(&self, token_id: &str) -> Result<CancelOrderResponse> {
        if self.dry_run {
            info!("DRY RUN: Would cancel all orders for token {}", token_id);
            return Ok(CancelOrderResponse {
                canceled: Some(vec![]),
                not_canceled: None,
            });
        }

        let signer = self
            .signer
            .as_ref()
            .ok_or_else(|| PloyError::Auth("Not authenticated".to_string()))?;

        let _guard = self.order_mutex.lock().await;
        let auth_client = self.authenticate_cached(signer).await?;

        let token_u256 = U256::from_str(token_id)
            .map_err(|e| PloyError::Internal(format!("Invalid token_id '{}': {}", token_id, e)))?;

        let req = CancelMarketOrderRequest::builder()
            .asset_id(token_u256)
            .build();
        let resp = auth_client
            .cancel_market_orders(&req)
            .await
            .map_err(|e| PloyError::Internal(format!("Failed to cancel token orders: {}", e)))?;

        let not_canceled = if resp.not_canceled.is_empty() {
            None
        } else {
            Some(
                resp.not_canceled
                    .into_iter()
                    .map(|(order_id, reason)| NotCanceledOrder { order_id, reason })
                    .collect(),
            )
        };

        Ok(CancelOrderResponse {
            canceled: Some(resp.canceled),
            not_canceled,
        })
    }

    // ==================== Account Methods ====================

    /// Get account balance
    #[instrument(skip(self))]
    pub async fn get_balance(&self) -> Result<BalanceResponse> {
        if self.dry_run {
            return Ok(BalanceResponse {
                balance: "100.00".to_string(),
                allowance: None,
            });
        }

        let signer = self
            .signer
            .as_ref()
            .ok_or_else(|| PloyError::Auth("Not authenticated".to_string()))?;

        let _guard = self.order_mutex.lock().await;
        let auth_client = self.authenticate_cached(signer).await?;

        let req = BalanceAllowanceRequest::builder()
            .asset_type(AssetType::Collateral)
            .build();

        let resp = auth_client
            .balance_allowance(req)
            .await
            .map_err(|e| PloyError::Internal(format!("Failed to get balance: {}", e)))?;

        Ok(BalanceResponse {
            balance: resp.balance.to_string(),
            allowance: None,
        })
    }

    /// Get USDC balance
    #[instrument(skip(self))]
    pub async fn get_usdc_balance(&self) -> Result<Decimal> {
        let balance = self.get_balance().await?;
        balance
            .balance
            .parse::<Decimal>()
            .map_err(|e| PloyError::Internal(format!("Failed to parse balance: {}", e)))
    }

    /// Get open orders
    #[instrument(skip(self))]
    pub async fn get_open_orders(&self) -> Result<Vec<OrderResponse>> {
        if self.dry_run {
            return Ok(vec![]);
        }

        let signer = self
            .signer
            .as_ref()
            .ok_or_else(|| PloyError::Auth("Not authenticated".to_string()))?;

        let _guard = self.order_mutex.lock().await;
        let auth_client = self.authenticate_cached(signer).await?;

        let req = OrdersRequest::builder().build();

        let orders = self
            .fetch_orders_paginated(&auth_client, &req, None)
            .await?;

        // Filter for open orders (LIVE status)
        Ok(orders
            .into_iter()
            .filter(|o| {
                let status = format!("{:?}", o.status);
                status.contains("Live") || status.contains("Open")
            })
            .map(|o| OrderResponse {
                id: o.id.clone(),
                status: format!("{:?}", o.status),
                owner: Some(o.owner.to_string()),
                market: Some(o.market.to_string()),
                asset_id: Some(o.asset_id.to_string()),
                side: Some(format!("{:?}", o.side)),
                original_size: Some(o.original_size.to_string()),
                size_matched: Some(o.size_matched.to_string()),
                price: Some(o.price.to_string()),
                associate_trades: None,
                created_at: Some(o.created_at.to_rfc3339()),
                expiration: Some(o.expiration.to_rfc3339()),
                order_type: Some(format!("{:?}", o.order_type)),
            })
            .collect())
    }

    /// Get orders for a specific token
    #[instrument(skip(self))]
    pub async fn get_orders_for_token(&self, token_id: &str) -> Result<Vec<OrderResponse>> {
        if self.dry_run {
            return Ok(vec![]);
        }

        let signer = self
            .signer
            .as_ref()
            .ok_or_else(|| PloyError::Auth("Not authenticated".to_string()))?;

        let _guard = self.order_mutex.lock().await;
        let auth_client = self.authenticate_cached(signer).await?;

        let token_u256 = U256::from_str(token_id)
            .map_err(|e| PloyError::Internal(format!("Invalid token_id '{}': {}", token_id, e)))?;

        let req = OrdersRequest::builder().asset_id(token_u256).build();

        let orders = self
            .fetch_orders_paginated(&auth_client, &req, None)
            .await?;

        Ok(orders
            .into_iter()
            .map(|o| OrderResponse {
                id: o.id.clone(),
                status: format!("{:?}", o.status),
                owner: Some(o.owner.to_string()),
                market: Some(o.market.to_string()),
                asset_id: Some(o.asset_id.to_string()),
                side: Some(format!("{:?}", o.side)),
                original_size: Some(o.original_size.to_string()),
                size_matched: Some(o.size_matched.to_string()),
                price: Some(o.price.to_string()),
                associate_trades: None,
                created_at: Some(o.created_at.to_rfc3339()),
                expiration: Some(o.expiration.to_rfc3339()),
                order_type: Some(format!("{:?}", o.order_type)),
            })
            .collect())
    }

    /// Get order history
    #[instrument(skip(self))]
    pub async fn get_order_history(&self, limit: Option<u32>) -> Result<Vec<OrderResponse>> {
        if self.dry_run {
            return Ok(vec![]);
        }

        let signer = self
            .signer
            .as_ref()
            .ok_or_else(|| PloyError::Auth("Not authenticated".to_string()))?;

        let _guard = self.order_mutex.lock().await;
        let auth_client = self.authenticate_cached(signer).await?;

        let req = OrdersRequest::builder().build();
        let orders_data = self
            .fetch_orders_paginated(&auth_client, &req, limit.map(|v| v as usize))
            .await?;

        Ok(orders_data
            .into_iter()
            .map(|o| OrderResponse {
                id: o.id.clone(),
                status: format!("{:?}", o.status),
                owner: Some(o.owner.to_string()),
                market: Some(o.market.to_string()),
                asset_id: Some(o.asset_id.to_string()),
                side: Some(format!("{:?}", o.side)),
                original_size: Some(o.original_size.to_string()),
                size_matched: Some(o.size_matched.to_string()),
                price: Some(o.price.to_string()),
                associate_trades: None,
                created_at: Some(o.created_at.to_rfc3339()),
                expiration: Some(o.expiration.to_rfc3339()),
                order_type: Some(format!("{:?}", o.order_type)),
            })
            .collect())
    }

    /// Get positions (via Polymarket Data API)
    #[instrument(skip(self))]
    pub async fn get_positions(&self) -> Result<Vec<PositionResponse>> {
        if self.dry_run {
            return Ok(vec![]);
        }

        let user = if let Some(funder) = self.funder {
            format!("{:#x}", funder)
        } else if let Some(w) = self.wallet.as_ref() {
            format!("{:#x}", w.address())
        } else if let Some(signer) = self.signer.as_ref() {
            format!("{:#x}", signer.address())
        } else {
            return Err(PloyError::Auth("Not authenticated".to_string()));
        };

        let data_client = DataClient::default();
        let user_addr: polymarket_client_sdk::types::Address = user
            .parse()
            .map_err(|e| PloyError::Internal(format!("Invalid user address {}: {}", user, e)))?;

        let mut positions = Vec::new();
        let mut offset: i32 = 0;
        let page_size: i32 = 500;

        loop {
            let req_builder = PositionsRequest::builder().user(user_addr);
            let req_builder = req_builder
                .limit(page_size)
                .map_err(|e| PloyError::Internal(format!("Invalid positions limit: {}", e)))?;
            let req_builder = req_builder
                .offset(offset)
                .map_err(|e| PloyError::Internal(format!("Invalid positions offset: {}", e)))?;
            let req = req_builder.build();

            let batch = data_client
                .positions(&req)
                .await
                .map_err(|e| PloyError::Internal(format!("Failed to fetch positions: {}", e)))?;

            if batch.is_empty() {
                break;
            }

            let batch_len = batch.len() as i32;
            positions.extend(batch.into_iter().map(|p| {
                let mut extra = HashMap::new();
                extra.insert("title".to_string(), serde_json::json!(p.title));
                extra.insert("slug".to_string(), serde_json::json!(p.slug));
                extra.insert("eventSlug".to_string(), serde_json::json!(p.event_slug));
                extra.insert(
                    "oppositeOutcome".to_string(),
                    serde_json::json!(p.opposite_outcome),
                );
                extra.insert(
                    "oppositeAsset".to_string(),
                    serde_json::json!(p.opposite_asset),
                );
                extra.insert("endDate".to_string(), serde_json::json!(p.end_date));
                extra.insert("mergeable".to_string(), serde_json::json!(p.mergeable));

                PositionResponse {
                    asset_id: p.asset.to_string(),
                    token_id: Some(p.asset.to_string()),
                    condition_id: Some(p.condition_id.to_string()),
                    outcome: Some(p.outcome),
                    outcome_index: Some(p.outcome_index.to_string()),
                    size: p.size.to_string(),
                    avg_price: Some(p.avg_price.to_string()),
                    realized_pnl: Some(p.realized_pnl.to_string()),
                    unrealized_pnl: Some(p.cash_pnl.to_string()),
                    cur_price: Some(p.cur_price.to_string()),
                    redeemable: Some(p.redeemable),
                    negative_risk: Some(p.negative_risk),
                    extra,
                }
            }));

            if batch_len < page_size || offset >= 10_000 {
                break;
            }
            offset += batch_len;
        }

        Ok(positions)
    }

    /// Get trades
    #[instrument(skip(self))]
    pub async fn get_trades(&self, limit: Option<u32>) -> Result<Vec<TradeResponse>> {
        if self.dry_run {
            return Ok(vec![]);
        }

        let signer = self
            .signer
            .as_ref()
            .ok_or_else(|| PloyError::Auth("Not authenticated".to_string()))?;

        let _guard = self.order_mutex.lock().await;
        let auth_client = self.authenticate_cached(signer).await?;

        let req = TradesRequest::builder().build();

        let trades = self
            .fetch_trades_paginated(&auth_client, &req, limit.map(|v| v as usize))
            .await?;

        Ok(trades
            .into_iter()
            .map(|t| TradeResponse {
                id: Some(t.id.clone()),
                order_id: Some(t.taker_order_id.clone()),
                asset_id: t.asset_id.to_string(),
                side: format!("{:?}", t.side),
                price: t.price.to_string(),
                size: t.size.to_string(),
                fee: Some(t.fee_rate_bps.to_string()),
                timestamp: Some(t.match_time.to_rfc3339()),
                extra: HashMap::new(),
            })
            .collect())
    }

    /// Get comprehensive account summary
    #[instrument(skip(self))]
    pub async fn get_account_summary(&self) -> Result<AccountSummary> {
        let usdc_balance = self.get_usdc_balance().await.unwrap_or(Decimal::ZERO);
        let open_orders = self.get_open_orders().await.unwrap_or_default();
        let positions = self.get_positions().await.unwrap_or_default();

        let open_order_value = open_orders
            .iter()
            .filter_map(|o| {
                let price = o.price.as_ref()?.parse::<Decimal>().ok()?;
                let size = o.original_size.as_ref()?.parse::<Decimal>().ok()?;
                Some(price * size)
            })
            .sum();

        let position_value = positions.iter().filter_map(|p| p.market_value()).sum();

        let total_equity = usdc_balance + position_value;

        Ok(AccountSummary {
            usdc_balance,
            open_order_count: open_orders.len(),
            open_order_value,
            position_count: positions.len(),
            position_value,
            total_equity,
            open_orders,
            positions,
        })
    }
}
