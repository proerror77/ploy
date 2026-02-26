//! `ploy pm orders` — Order management commands (authenticated).

use clap::Subcommand;

use super::auth::PmAuth;
use super::output::{self, OutputMode};
use super::GlobalPmArgs;

#[derive(Subcommand, Debug, Clone)]
pub enum OrdersCommands {
    /// Create a limit order.
    Create {
        #[arg(long)]
        token_id: String,
        #[arg(long)]
        side: String,
        #[arg(long)]
        price: String,
        #[arg(long)]
        size: String,
    },
    /// Place a market buy order.
    MarketBuy {
        #[arg(long)]
        token_id: String,
        #[arg(long)]
        amount: String,
    },
    /// Place a market sell order.
    MarketSell {
        #[arg(long)]
        token_id: String,
        #[arg(long)]
        size: String,
    },
    /// List open orders.
    List {
        #[arg(long)]
        market: Option<String>,
    },
    /// Get order details.
    Get { order_id: String },
    /// Cancel an order.
    Cancel { order_id: String },
    /// Cancel all orders.
    CancelAll {
        #[arg(long)]
        market: Option<String>,
    },
    /// List recent trades.
    Trades {
        #[arg(long)]
        market: Option<String>,
    },
}

pub async fn run(
    cmd: OrdersCommands,
    auth: &PmAuth,
    mode: OutputMode,
    args: &GlobalPmArgs,
) -> anyhow::Result<()> {
    use alloy::primitives::{B256, U256};
    use polymarket_client_sdk::clob::types::request::*;
    use polymarket_client_sdk::clob::types::{Amount, Side, SignatureType};
    use polymarket_client_sdk::clob::Client as ClobClient;
    use rust_decimal::Decimal;
    use std::str::FromStr;

    let signer = auth.require_signer()?;
    let config = super::config_file::PmConfig::load().unwrap_or_default();

    let mut auth_builder = ClobClient::new(
        config.clob_base_url(),
        polymarket_client_sdk::clob::Config::default(),
    )?
    .authentication_builder(signer);

    if let Some(funder) = auth.funder {
        auth_builder = auth_builder
            .funder(funder)
            .signature_type(SignatureType::Proxy);
    }

    let client = auth_builder.authenticate().await?;

    match cmd {
        OrdersCommands::Create {
            token_id,
            side,
            price,
            size,
        } => {
            let tid = U256::from_str(&token_id)?;
            let sdk_side = match side.to_uppercase().as_str() {
                "BUY" | "B" => Side::Buy,
                "SELL" | "S" => Side::Sell,
                other => anyhow::bail!("invalid side '{other}': expected BUY or SELL"),
            };
            let price_dec = Decimal::from_str(&price)?;
            let size_dec = Decimal::from_str(&size)?;

            if args.dry_run {
                output::print_warn(&format!(
                    "[DRY RUN] Would create {side} order: token={token_id} price={price} size={size}"
                ));
                return Ok(());
            }
            if !args.yes
                && !output::confirm(&format!(
                    "Create {side} order: token={token_id} price={price} size={size}?"
                ))
            {
                output::print_warn("cancelled");
                return Ok(());
            }

            let signable = client
                .limit_order()
                .token_id(tid)
                .side(sdk_side)
                .price(price_dec)
                .size(size_dec)
                .build()
                .await?;
            let signed = client.sign(signer, signable).await?;
            let resp = client.post_order(signed).await?;
            output::print_debug(&resp, mode)?;
        }
        OrdersCommands::MarketBuy { token_id, amount } => {
            let tid = U256::from_str(&token_id)?;
            let amount_dec = Decimal::from_str(&amount)?;

            if args.dry_run {
                output::print_warn(&format!(
                    "[DRY RUN] Would market buy: token={token_id} amount={amount} USDC"
                ));
                return Ok(());
            }
            if !args.yes
                && !output::confirm(&format!(
                    "Market buy: token={token_id} amount={amount} USDC?"
                ))
            {
                output::print_warn("cancelled");
                return Ok(());
            }

            let signable = client
                .market_order()
                .token_id(tid)
                .side(Side::Buy)
                .amount(Amount::usdc(amount_dec)?)
                .build()
                .await?;
            let signed = client.sign(signer, signable).await?;
            let resp = client.post_order(signed).await?;
            output::print_debug(&resp, mode)?;
        }
        OrdersCommands::MarketSell { token_id, size } => {
            let tid = U256::from_str(&token_id)?;
            let size_dec = Decimal::from_str(&size)?;

            if args.dry_run {
                output::print_warn(&format!(
                    "[DRY RUN] Would market sell: token={token_id} size={size}"
                ));
                return Ok(());
            }
            if !args.yes && !output::confirm(&format!("Market sell: token={token_id} size={size}?"))
            {
                output::print_warn("cancelled");
                return Ok(());
            }

            let signable = client
                .market_order()
                .token_id(tid)
                .side(Side::Sell)
                .amount(Amount::shares(size_dec)?)
                .build()
                .await?;
            let signed = client.sign(signer, signable).await?;
            let resp = client.post_order(signed).await?;
            output::print_debug(&resp, mode)?;
        }
        OrdersCommands::List { market } => {
            let market_b256 = market.as_deref().map(B256::from_str).transpose()?;
            let req = OrdersRequest::builder().maybe_market(market_b256).build();
            let page = client.orders(&req, None).await?;
            output::print_debug_items(&page.data, mode)?;
        }
        OrdersCommands::Get { order_id } => {
            let req = OrdersRequest::builder().order_id(order_id).build();
            let page = client.orders(&req, None).await?;
            output::print_debug_items(&page.data, mode)?;
        }
        OrdersCommands::Cancel { order_id } => {
            if args.dry_run {
                output::print_warn(&format!("[DRY RUN] Would cancel order: {order_id}"));
                return Ok(());
            }
            if !args.yes && !output::confirm(&format!("Cancel order {order_id}?")) {
                output::print_warn("cancelled");
                return Ok(());
            }
            let resp = client.cancel_order(&order_id).await?;
            output::print_debug(&resp, mode)?;
            output::print_success(&format!("order {order_id} cancelled"));
        }
        OrdersCommands::CancelAll { market } => {
            if args.dry_run {
                output::print_warn("[DRY RUN] Would cancel all orders");
                return Ok(());
            }
            if !args.yes && !output::confirm("Cancel ALL orders? This cannot be undone.") {
                output::print_warn("cancelled");
                return Ok(());
            }
            if let Some(m) = market {
                let b256 = B256::from_str(&m)?;
                let req = CancelMarketOrderRequest::builder().market(b256).build();
                let resp = client.cancel_market_orders(&req).await?;
                output::print_debug(&resp, mode)?;
            } else {
                let resp = client.cancel_all_orders().await?;
                output::print_debug(&resp, mode)?;
            }
        }
        OrdersCommands::Trades { market } => {
            let market_b256 = market.as_deref().map(B256::from_str).transpose()?;
            let req = TradesRequest::builder().maybe_market(market_b256).build();
            let page = client.trades(&req, None).await?;
            output::print_debug_items(&page.data, mode)?;
        }
    }
    Ok(())
}
