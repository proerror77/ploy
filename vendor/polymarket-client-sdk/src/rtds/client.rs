use std::sync::Arc;

use futures::Stream;
use futures::StreamExt as _;
use futures::future;

use super::subscription::{SimpleParser, SubscriptionManager, TopicType};
use super::types::request::Subscription;
use super::types::response::{
    ChainlinkPrice, Comment, CommentType, CryptoPrice, EquityPriceSnapshot, EquityPriceUpdate,
    RtdsMessage,
};
use crate::Result;
use crate::auth::state::{Authenticated, State, Unauthenticated};
use crate::auth::{Credentials, Normal};
use crate::error::Error;
use crate::types::Address;
use crate::ws::ConnectionManager;
use crate::ws::config::Config;
use crate::ws::connection::ConnectionState;

/// RTDS (Real-Time Data Socket) client for streaming Polymarket data.
///
/// - [`Client<Unauthenticated>`]: All streams, comments without auth
/// - [`Client<Authenticated<Normal>>`]: All streams, comments with CLOB auth
///
/// # Examples
///
/// ```rust, no_run
/// use polymarket_client_sdk::rtds::Client;
/// use futures::StreamExt;
///
/// #[tokio::main]
/// async fn main() -> anyhow::Result<()> {
///     let client = Client::default();
///
///     // Subscribe to BTC and ETH prices from Binance
///     let symbols = vec!["btcusdt".to_owned(), "ethusdt".to_owned()];
///     let stream = client.subscribe_crypto_prices(Some(symbols))?;
///     let mut stream = Box::pin(stream);
///
///     while let Some(price) = stream.next().await {
///         println!("Price: {:?}", price?);
///     }
///
///     Ok(())
/// }
/// ```
#[derive(Clone)]
pub struct Client<S: State = Unauthenticated> {
    inner: Arc<ClientInner<S>>,
}

impl Default for Client<Unauthenticated> {
    fn default() -> Self {
        Self::new("wss://ws-live-data.polymarket.com", Config::default())
            .expect("RTDS client with default endpoint should succeed")
    }
}

struct ClientInner<S: State> {
    /// Current state of the client
    state: S,
    /// Configuration for the RTDS connection
    config: Config,
    /// Base endpoint for the WebSocket
    endpoint: String,
    /// Connection manager for the WebSocket
    connection: ConnectionManager<RtdsMessage, SimpleParser>,
    /// Subscription manager for handling subscriptions
    subscriptions: Arc<SubscriptionManager>,
}

impl Client<Unauthenticated> {
    /// Create a new unauthenticated RTDS client with the specified endpoint and configuration.
    pub fn new(endpoint: &str, config: Config) -> Result<Self> {
        let connection = ConnectionManager::new(endpoint.to_owned(), config.clone(), SimpleParser)?;
        let subscriptions = Arc::new(SubscriptionManager::new(connection.clone()));

        // Start reconnection handler to re-subscribe on connection recovery
        subscriptions.start_reconnection_handler();

        Ok(Self {
            inner: Arc::new(ClientInner {
                state: Unauthenticated,
                config,
                endpoint: endpoint.to_owned(),
                connection,
                subscriptions,
            }),
        })
    }

    /// Authenticate with CLOB credentials.
    ///
    /// Returns an authenticated client that can subscribe to comments with auth.
    pub fn authenticate(
        self,
        address: Address,
        credentials: Credentials,
    ) -> Result<Client<Authenticated<Normal>>> {
        let inner = Arc::into_inner(self.inner).ok_or(Error::validation(
            "Cannot authenticate while other references to this client exist",
        ))?;

        Ok(Client {
            inner: Arc::new(ClientInner {
                state: Authenticated {
                    address,
                    credentials,
                    kind: Normal,
                },
                config: inner.config,
                endpoint: inner.endpoint,
                connection: inner.connection,
                subscriptions: inner.subscriptions,
            }),
        })
    }

    /// Subscribe to comment events (unauthenticated).
    ///
    /// # Arguments
    ///
    /// * `comment_type` - Optional comment event type to filter
    pub fn subscribe_comments(
        &self,
        comment_type: Option<CommentType>,
    ) -> Result<impl Stream<Item = Result<Comment>>> {
        let subscription = Subscription::comments(comment_type);
        let stream = self.inner.subscriptions.subscribe(subscription)?;

        Ok(stream.filter_map(|msg_result| async move {
            match msg_result {
                Ok(msg) => msg.as_comment().map(Ok),
                Err(e) => Some(Err(e)),
            }
        }))
    }
}

// Methods available in any state
impl<S: State> Client<S> {
    /// Subscribes to real-time cryptocurrency price updates from Binance.
    ///
    /// Returns a stream of cryptocurrency prices for the specified trading pairs.
    /// If no symbols are provided, subscribes to all available cryptocurrency pairs.
    /// Prices are sourced from Binance and updated in real-time.
    ///
    /// # Arguments
    ///
    /// * `symbols` - Optional list of trading pair symbols (e.g., `["BTCUSDT", "ETHUSDT"]`).
    ///   If `None`, subscribes to all available pairs.
    ///
    /// # Errors
    ///
    /// Returns an error if the subscription cannot be created or the WebSocket
    /// connection fails.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use polymarket_client_sdk::rtds::Client;
    /// use polymarket_client_sdk::ws::config::Config;
    /// use futures::StreamExt;
    /// use tokio::pin;
    ///
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// let client = Client::new("wss://rtds.polymarket.com", Config::default())?;
    /// let stream = client.subscribe_crypto_prices(Some(vec!["BTCUSDT".to_string()]))?;
    ///
    /// pin!(stream);
    ///
    /// while let Some(price_result) = stream.next().await {
    ///     let price = price_result?;
    ///     println!("BTC Price: ${}", price.value);
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub fn subscribe_crypto_prices(
        &self,
        symbols: Option<Vec<String>>,
    ) -> Result<impl Stream<Item = Result<CryptoPrice>>> {
        let requested_symbols = symbols.clone().map(|symbols| {
            symbols
                .into_iter()
                .map(|symbol| symbol.trim().to_lowercase())
                .filter(|symbol| !symbol.is_empty())
                .collect::<std::collections::HashSet<_>>()
        });
        let subscriptions = Subscription::crypto_price_subscriptions(symbols);
        let stream = self.inner.subscriptions.subscribe_many(subscriptions)?;

        Ok(stream.filter_map(move |msg_result| {
            let requested_symbols = requested_symbols.clone();
            future::ready(match msg_result {
                Ok(msg) => match msg.as_crypto_price() {
                    Some(price) => {
                        let matches_request = requested_symbols
                            .as_ref()
                            .is_none_or(|symbols| symbols.contains(&price.symbol.to_lowercase()));
                        matches_request.then_some(Ok(price))
                    }
                    None => None,
                },
                Err(e) => Some(Err(e)),
            })
        }))
    }

    /// Subscribe to Chainlink price feed updates.
    pub fn subscribe_chainlink_prices(
        &self,
        symbol: Option<String>,
    ) -> Result<impl Stream<Item = Result<ChainlinkPrice>>> {
        let subscription = Subscription::chainlink_prices(symbol);
        let stream = self.inner.subscriptions.subscribe(subscription)?;

        Ok(stream.filter_map(|msg_result| async move {
            match msg_result {
                Ok(msg) => msg.as_chainlink_price().map(Ok),
                Err(e) => Some(Err(e)),
            }
        }))
    }

    /// Subscribe to Pyth-backed equity, forex, commodity, and metal prices.
    pub fn subscribe_equity_prices(
        &self,
        symbol: Option<String>,
        include_snapshot: bool,
    ) -> Result<impl Stream<Item = Result<EquityPriceMessage>>> {
        let msg_type = if include_snapshot { "*" } else { "update" };
        let subscription = Subscription::equity_prices(symbol, msg_type);
        let stream = self.inner.subscriptions.subscribe(subscription)?;

        Ok(stream.filter_map(|msg_result| async move {
            match msg_result {
                Ok(msg) => {
                    if let Some(update) = msg.as_equity_price_update() {
                        Some(Ok(EquityPriceMessage::Update(update)))
                    } else {
                        msg.as_equity_price_snapshot()
                            .map(EquityPriceMessage::Snapshot)
                            .map(Ok)
                    }
                }
                Err(e) => Some(Err(e)),
            }
        }))
    }

    /// Subscribe to raw RTDS messages for a custom topic/type combination.
    pub fn subscribe_raw(
        &self,
        subscription: Subscription,
    ) -> Result<impl Stream<Item = Result<RtdsMessage>>> {
        self.inner.subscriptions.subscribe(subscription)
    }

    /// Get the current connection state.
    ///
    /// # Returns
    ///
    /// The current [`ConnectionState`] of the WebSocket connection.
    #[must_use]
    pub fn connection_state(&self) -> ConnectionState {
        self.inner.connection.state()
    }

    /// Get the number of active subscriptions.
    ///
    /// # Returns
    ///
    /// The count of active subscriptions managed by this client.
    #[must_use]
    pub fn subscription_count(&self) -> usize {
        self.inner.subscriptions.subscription_count()
    }

    /// Unsubscribe from Binance crypto price updates.
    ///
    /// This decrements the reference count for the `crypto_prices` topic. Only sends
    /// an unsubscribe request to the server when no other streams are using this topic.
    ///
    /// # Errors
    ///
    /// Returns an error if the unsubscribe request fails.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use polymarket_client_sdk::rtds::Client;
    ///
    /// # fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// let client = Client::default();
    /// let _stream = client.subscribe_crypto_prices(None)?;
    /// // Later...
    /// client.unsubscribe_crypto_prices()?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn unsubscribe_crypto_prices(&self) -> Result<()> {
        let topic = TopicType::new("crypto_prices".to_owned(), "update".to_owned());
        self.inner.subscriptions.unsubscribe(&[topic])
    }

    /// Unsubscribe from Chainlink price feed updates.
    ///
    /// This decrements the reference count for the chainlink topic. Only sends
    /// an unsubscribe request to the server when no other streams are using this topic.
    ///
    /// # Errors
    ///
    /// Returns an error if the unsubscribe request fails.
    pub fn unsubscribe_chainlink_prices(&self) -> Result<()> {
        let topic = TopicType::new("crypto_prices_chainlink".to_owned(), "*".to_owned());
        self.inner.subscriptions.unsubscribe(&[topic])
    }

    /// Unsubscribe from Pyth-backed equity and commodity price updates.
    pub fn unsubscribe_equity_prices(&self, include_snapshot: bool) -> Result<()> {
        let msg_type = if include_snapshot { "*" } else { "update" };
        let topic = TopicType::new("equity_prices".to_owned(), msg_type.to_owned());
        self.inner.subscriptions.unsubscribe(&[topic])
    }

    /// Unsubscribe from comment events.
    ///
    /// # Arguments
    ///
    /// * `comment_type` - The comment type to unsubscribe from. Use `None` for wildcard (`*`).
    pub fn unsubscribe_comments(&self, comment_type: Option<CommentType>) -> Result<()> {
        let msg_type = comment_type.map_or("*".to_owned(), |t| {
            serde_json::to_string(&t)
                .ok()
                .and_then(|s| s.trim_matches('"').to_owned().into())
                .unwrap_or_else(|| "*".to_owned())
        });
        let topic = TopicType::new("comments".to_owned(), msg_type);
        self.inner.subscriptions.unsubscribe(&[topic])
    }
}

/// Typed RTDS equity feed messages.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub enum EquityPriceMessage {
    /// Live RTDS price update (`type = "update"`)
    Update(EquityPriceUpdate),
    /// Initial subscribe/backfill snapshot (`type = "subscribe"`)
    Snapshot(EquityPriceSnapshot),
}

impl Client<Authenticated<Normal>> {
    /// Subscribe to comment events with CLOB authentication.
    ///
    /// # Arguments
    ///
    /// * `comment_type` - Optional comment event type to filter
    pub fn subscribe_comments(
        &self,
        comment_type: Option<CommentType>,
    ) -> Result<impl Stream<Item = Result<Comment>>> {
        let subscription = Subscription::comments(comment_type)
            .with_clob_auth(self.inner.state.credentials.clone());
        let stream = self.inner.subscriptions.subscribe(subscription)?;

        Ok(stream.filter_map(|msg_result| async move {
            match msg_result {
                Ok(msg) => msg.as_comment().map(Ok),
                Err(e) => Some(Err(e)),
            }
        }))
    }

    /// Deauthenticate and return to unauthenticated state.
    pub fn deauthenticate(self) -> Result<Client<Unauthenticated>> {
        let inner = Arc::into_inner(self.inner).ok_or(Error::validation(
            "Cannot deauthenticate while other references to this client exist",
        ))?;

        Ok(Client {
            inner: Arc::new(ClientInner {
                state: Unauthenticated,
                config: inner.config,
                endpoint: inner.endpoint,
                connection: inner.connection,
                subscriptions: inner.subscriptions,
            }),
        })
    }
}
