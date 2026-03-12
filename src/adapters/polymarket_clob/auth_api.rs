use super::*;

impl PolymarketClient {
    pub(in crate::adapters::polymarket_clob) async fn fetch_orders_paginated(
        &self,
        auth_client: &AuthClobClient,
        req: &OrdersRequest,
        limit: Option<usize>,
    ) -> Result<Vec<polymarket_client_sdk::clob::types::response::OpenOrderResponse>> {
        let mut cursor: Option<String> = None;
        let mut out = Vec::new();

        loop {
            let page = auth_client
                .orders(req, cursor.clone())
                .await
                .map_err(|e| PloyError::Internal(format!("Failed to get orders: {}", e)))?;

            for order in page.data {
                out.push(order);
                if let Some(max) = limit {
                    if out.len() >= max {
                        return Ok(out);
                    }
                }
            }

            if page.next_cursor == CLOB_TERMINAL_CURSOR {
                break;
            }
            cursor = Some(page.next_cursor);
        }

        Ok(out)
    }

    pub(in crate::adapters::polymarket_clob) async fn fetch_trades_paginated(
        &self,
        auth_client: &AuthClobClient,
        req: &TradesRequest,
        limit: Option<usize>,
    ) -> Result<Vec<polymarket_client_sdk::clob::types::response::TradeResponse>> {
        let mut cursor: Option<String> = None;
        let mut out = Vec::new();

        loop {
            let page = auth_client
                .trades(req, cursor.clone())
                .await
                .map_err(|e| PloyError::Internal(format!("Failed to get trades: {}", e)))?;

            for trade in page.data {
                out.push(trade);
                if let Some(max) = limit {
                    if out.len() >= max {
                        return Ok(out);
                    }
                }
            }

            if page.next_cursor == CLOB_TERMINAL_CURSOR {
                break;
            }
            cursor = Some(page.next_cursor);
        }

        Ok(out)
    }

    #[allow(dead_code)]
    async fn clear_cached_auth(&self) {
        let mut guard = self.auth_client.lock().await;
        *guard = None;
    }

    pub(in crate::adapters::polymarket_clob) async fn authenticate_new(
        &self,
        signer: &PrivateKeySigner,
    ) -> Result<AuthClobClient> {
        let fresh_client = ClobClient::new(&self.base_url, ClobConfig::default())
            .map_err(|e| PloyError::Internal(format!("Failed to create CLOB client: {}", e)))?;

        let auth_client = if let Some(funder) = self.funder {
            debug!("Using proxy wallet authentication, funder: {:?}", funder);
            fresh_client
                .authentication_builder(signer)
                .funder(funder)
                .signature_type(SdkSignatureType::Proxy)
                .authenticate()
                .await
                .map_err(|e| PloyError::Auth(format!("Proxy authentication failed: {}", e)))?
        } else {
            debug!("Using EOA wallet authentication");
            fresh_client
                .authentication_builder(signer)
                .authenticate()
                .await
                .map_err(|e| PloyError::Auth(format!("Authentication failed: {}", e)))?
        };

        Ok(auth_client)
    }

    pub(in crate::adapters::polymarket_clob) async fn authenticate_cached(
        &self,
        signer: &PrivateKeySigner,
    ) -> Result<AuthClobClient> {
        {
            let guard = self.auth_client.lock().await;
            if let Some(client) = guard.as_ref() {
                return Ok(client.clone());
            }
        }

        let mut backoff_ms: u64 = 250;
        let mut last_err: Option<PloyError> = None;
        for attempt in 0..3 {
            match self.authenticate_new(signer).await {
                Ok(client) => {
                    let mut guard = self.auth_client.lock().await;
                    *guard = Some(client.clone());
                    return Ok(client);
                }
                Err(e) => {
                    warn!(
                        attempt = attempt + 1,
                        backoff_ms,
                        error = %e,
                        "Polymarket authentication handshake failed"
                    );
                    last_err = Some(e);
                    if attempt < 2 {
                        tokio::time::sleep(tokio::time::Duration::from_millis(backoff_ms)).await;
                        backoff_ms = (backoff_ms * 2).min(5_000);
                    }
                }
            }
        }

        Err(last_err
            .unwrap_or_else(|| PloyError::Auth("Polymarket authentication failed".to_string())))
    }

    pub fn new(base_url: &str, dry_run: bool) -> Result<Self> {
        let config = ClobConfig::default();
        let clob_client = ClobClient::new(base_url, config)
            .map_err(|e| PloyError::Internal(format!("Failed to create CLOB client: {}", e)))?;

        let gamma_client = GammaClient::new(GAMMA_API_URL)
            .map_err(|e| PloyError::Internal(format!("Failed to create Gamma client: {}", e)))?;

        info!(
            "Created Polymarket SDK client (read-only, dry_run={})",
            dry_run
        );

        Ok(Self {
            clob_client,
            gamma_client,
            signer: None,
            wallet: None,
            funder: None,
            base_url: base_url.trim_end_matches('/').to_string(),
            dry_run,
            neg_risk: false,
            order_mutex: Arc::new(Mutex::new(())),
            auth_client: Arc::new(Mutex::new(None)),
        })
    }

    /// Create an authenticated CLOB client with wallet
    /// For proxy wallets (Magic/email), use new_authenticated_proxy instead
    pub async fn new_authenticated(base_url: &str, wallet: Wallet, neg_risk: bool) -> Result<Self> {
        let config = ClobConfig::default();
        let clob_client = ClobClient::new(base_url, config)
            .map_err(|e| PloyError::Internal(format!("Failed to create CLOB client: {}", e)))?;

        let gamma_client = GammaClient::new(GAMMA_API_URL)
            .map_err(|e| PloyError::Internal(format!("Failed to create Gamma client: {}", e)))?;

        let signer: PrivateKeySigner = {
            let mut private_key_hex = std::env::var("POLYMARKET_PRIVATE_KEY")
                .or_else(|_| std::env::var("PRIVATE_KEY"))
                .map_err(|_| {
                    PloyError::Wallet(
                        "POLYMARKET_PRIVATE_KEY or PRIVATE_KEY environment variable not set"
                            .to_string(),
                    )
                })?;

            let result = private_key_hex
                .trim_start_matches("0x")
                .parse::<PrivateKeySigner>()
                .map_err(|e| PloyError::Wallet(format!("Invalid private key: {}", e)));

            private_key_hex.zeroize();
            result?
        }
        .with_chain_id(Some(POLYGON_CHAIN_ID));

        info!(
            "Created authenticated Polymarket SDK client, address: {:?}",
            signer.address()
        );

        Ok(Self {
            clob_client,
            gamma_client,
            signer: Some(signer),
            wallet: Some(Arc::new(wallet)),
            funder: None,
            base_url: base_url.trim_end_matches('/').to_string(),
            dry_run: false,
            neg_risk,
            order_mutex: Arc::new(Mutex::new(())),
            auth_client: Arc::new(Mutex::new(None)),
        })
    }

    /// Create an authenticated CLOB client with proxy wallet (Magic/email wallet)
    /// funder_address is the proxy wallet address that holds the funds
    pub async fn new_authenticated_proxy(
        base_url: &str,
        wallet: Wallet,
        funder_address: &str,
        neg_risk: bool,
    ) -> Result<Self> {
        let config = ClobConfig::default();
        let clob_client = ClobClient::new(base_url, config)
            .map_err(|e| PloyError::Internal(format!("Failed to create CLOB client: {}", e)))?;

        let gamma_client = GammaClient::new(GAMMA_API_URL)
            .map_err(|e| PloyError::Internal(format!("Failed to create Gamma client: {}", e)))?;

        let signer: PrivateKeySigner = {
            let mut private_key_hex = std::env::var("POLYMARKET_PRIVATE_KEY")
                .or_else(|_| std::env::var("PRIVATE_KEY"))
                .map_err(|_| {
                    PloyError::Wallet(
                        "POLYMARKET_PRIVATE_KEY or PRIVATE_KEY environment variable not set"
                            .to_string(),
                    )
                })?;

            let result = private_key_hex
                .trim_start_matches("0x")
                .parse::<PrivateKeySigner>()
                .map_err(|e| PloyError::Wallet(format!("Invalid private key: {}", e)));

            private_key_hex.zeroize();
            result?
        }
        .with_chain_id(Some(POLYGON_CHAIN_ID));

        let funder: alloy::primitives::Address = funder_address
            .parse()
            .map_err(|e| PloyError::Wallet(format!("Invalid funder address: {}", e)))?;

        info!(
            "Created authenticated Polymarket SDK client (proxy mode), signer: {:?}, funder: {:?}",
            signer.address(),
            funder
        );

        Ok(Self {
            clob_client,
            gamma_client,
            signer: Some(signer),
            wallet: Some(Arc::new(wallet)),
            funder: Some(funder),
            base_url: base_url.trim_end_matches('/').to_string(),
            dry_run: false,
            neg_risk,
            order_mutex: Arc::new(Mutex::new(())),
            auth_client: Arc::new(Mutex::new(None)),
        })
    }

    pub fn set_funder(&mut self, funder_address: &str) -> Result<()> {
        let funder: alloy::primitives::Address = funder_address
            .parse()
            .map_err(|e| PloyError::Wallet(format!("Invalid funder address: {}", e)))?;
        self.funder = Some(funder);
        info!("Set funder address: {:?}", funder);
        Ok(())
    }

    pub fn is_dry_run(&self) -> bool {
        self.dry_run
    }

    pub fn has_hmac_auth(&self) -> bool {
        self.signer.is_some()
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }
}
