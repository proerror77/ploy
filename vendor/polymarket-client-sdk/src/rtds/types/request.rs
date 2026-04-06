use bon::Builder;
use secrecy::ExposeSecret as _;
use serde::Serialize;
use serde_json::Value;

use super::response::CommentType;
use crate::auth::Credentials;

/// RTDS subscription request message.
#[non_exhaustive]
#[derive(Clone, Debug, Serialize, Builder)]
pub struct SubscriptionRequest {
    /// Action type ("subscribe" or "unsubscribe")
    pub action: SubscriptionAction,
    /// List of subscriptions
    pub subscriptions: Vec<Subscription>,
}

impl SubscriptionRequest {
    /// Create a subscribe request.
    #[must_use]
    pub fn subscribe(subscriptions: Vec<Subscription>) -> Self {
        Self {
            action: SubscriptionAction::Subscribe,
            subscriptions,
        }
    }

    /// Create an unsubscribe request.
    #[must_use]
    pub fn unsubscribe(subscriptions: Vec<Subscription>) -> Self {
        Self {
            action: SubscriptionAction::Unsubscribe,
            subscriptions,
        }
    }
}

/// Subscription action type.
#[non_exhaustive]
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SubscriptionAction {
    /// Subscribe to topics
    Subscribe,
    /// Unsubscribe from topics
    Unsubscribe,
}

/// Individual subscription configuration.
///
/// # Security
///
/// When serialized, this struct exposes sensitive credentials (`clob_auth`) in plaintext.
/// Ensure subscription requests are only sent over secure WebSocket connections (`wss://`)
/// and never logged or exposed in error messages.
#[non_exhaustive]
#[derive(Clone, Debug, Builder)]
pub struct Subscription {
    /// Topic name (e.g., `crypto_prices`, `comments`)
    pub topic: String,
    /// Message type filter (e.g., `update`, `comment_created`, or `*` for all)
    pub msg_type: String,
    /// Optional filters (string or JSON object)
    pub filters: Option<String>,
    /// CLOB authentication (key, secret, passphrase)
    pub clob_auth: Option<Credentials>,
}

impl Subscription {
    /// Create a subscription for one Binance crypto symbol.
    #[must_use]
    pub fn crypto_prices(symbol: Option<String>) -> Self {
        let filters = symbol
            .map(|symbol| symbol.trim().to_lowercase())
            .filter(|symbol| !symbol.is_empty())
            .map(|symbol| format!(r#"{{"symbol":"{symbol}"}}"#));
        Self {
            topic: "crypto_prices".to_owned(),
            msg_type: "update".to_owned(),
            filters,
            clob_auth: None,
        }
    }

    /// Create one Binance subscription per requested symbol.
    ///
    /// RTDS currently validates `crypto_prices` filters as a JSON string containing
    /// exactly one symbol, for example `{"symbol":"btcusdt"}`. Multi-symbol filters
    /// inside one subscription are rejected by the server.
    #[must_use]
    pub fn crypto_price_subscriptions(symbols: Option<Vec<String>>) -> Vec<Self> {
        match symbols {
            Some(symbols) => {
                let subscriptions: Vec<Self> = symbols
                    .into_iter()
                    .map(|symbol| Self::crypto_prices(Some(symbol)))
                    .filter(|subscription| subscription.filters.is_some())
                    .collect();

                if subscriptions.is_empty() {
                    vec![Self::crypto_prices(None)]
                } else {
                    subscriptions
                }
            }
            None => vec![Self::crypto_prices(None)],
        }
    }

    /// Create a subscription for Chainlink crypto prices.
    #[must_use]
    pub fn chainlink_prices(symbol: Option<String>) -> Self {
        let filters = symbol.map(|s| format!(r#"{{"symbol":"{s}"}}"#));
        Self {
            topic: "crypto_prices_chainlink".to_owned(),
            msg_type: "*".to_owned(),
            filters,
            clob_auth: None,
        }
    }

    /// Create a subscription for Pyth-backed equity and commodity prices.
    #[must_use]
    pub fn equity_prices(symbol: Option<String>, msg_type: &str) -> Self {
        let filters = symbol.map(|s| format!(r#"{{"symbol":"{s}"}}"#));
        Self {
            topic: "equity_prices".to_owned(),
            msg_type: msg_type.to_owned(),
            filters,
            clob_auth: None,
        }
    }

    /// Create a subscription for comments.
    #[must_use]
    pub fn comments(msg_type: Option<CommentType>) -> Self {
        let type_str = msg_type.map_or("*".to_owned(), |t| {
            serde_json::to_string(&t)
                .ok()
                .and_then(|s| s.trim_matches('"').to_owned().into())
                .unwrap_or_else(|| "*".to_owned())
        });
        Self {
            topic: "comments".to_owned(),
            msg_type: type_str,
            filters: None,
            clob_auth: None,
        }
    }

    /// Set CLOB authentication for this subscription.
    #[must_use]
    pub fn with_clob_auth(mut self, credentials: Credentials) -> Self {
        self.clob_auth = Some(credentials);
        self
    }

    /// Set custom filters for this subscription.
    #[must_use]
    pub fn with_filters(mut self, filters: String) -> Self {
        self.filters = Some(filters);
        self
    }
}

// Custom Serialize implementation for Subscription to handle auth fields
impl Serialize for Subscription {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeMap as _;

        let mut map = serializer.serialize_map(None)?;

        map.serialize_entry("topic", &self.topic)?;
        map.serialize_entry("type", &self.msg_type)?;

        if let Some(filters) = &self.filters {
            // RTDS currently expects these topics to receive filters as JSON strings
            // rather than raw nested JSON objects.
            if self.topic == "crypto_prices"
                || self.topic == "crypto_prices_chainlink"
                || self.topic == "equity_prices"
            {
                map.serialize_entry("filters", filters)?;
            } else if let Ok(json_value) = serde_json::from_str::<Value>(filters) {
                // Other topics: parse and emit as raw JSON, e.g. ["btcusdt","ethusdt"]
                map.serialize_entry("filters", &json_value)?;
            } else {
                // Fallback: emit as string if not valid JSON
                map.serialize_entry("filters", filters)?;
            }
        }

        // SECURITY: Credentials are intentionally revealed here for the WebSocket auth protocol.
        // This data is only sent over wss:// connections to the RTDS server.
        if let Some(creds) = &self.clob_auth {
            let auth = serde_json::json!({
                "key": creds.key.to_string(),
                "secret": creds.secret.expose_secret(),
                "passphrase": creds.passphrase.expose_secret(),
            });
            map.serialize_entry("clob_auth", &auth)?;
        }

        map.end()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialize_subscription_request() {
        let subs = Subscription::crypto_price_subscriptions(Some(vec![
            "BTCUSDT".to_owned(),
            "ethusdt".to_owned(),
        ]));
        let request = SubscriptionRequest::subscribe(subs);

        let json = serde_json::to_string(&request).unwrap();
        assert!(json.contains("\"action\":\"subscribe\""));
        assert!(json.contains("\"topic\":\"crypto_prices\""));
        assert_eq!(json.matches("\"topic\":\"crypto_prices\"").count(), 2);
        assert!(json.contains(r#""filters":"{\"symbol\":\"btcusdt\"}""#));
        assert!(json.contains(r#""filters":"{\"symbol\":\"ethusdt\"}""#));
    }

    #[test]
    fn serialize_chainlink_subscription() {
        let sub = Subscription::chainlink_prices(Some("eth/usd".to_owned()));
        let request = SubscriptionRequest::subscribe(vec![sub]);

        let json = serde_json::to_string(&request).unwrap();
        assert!(json.contains("\"topic\":\"crypto_prices_chainlink\""));
        assert!(json.contains("\"type\":\"*\""));
        // Chainlink filters should be a JSON string (escaped), not a raw JSON object
        // See: https://github.com/Polymarket/rs-clob-client/issues/136
        assert!(
            json.contains(r#""filters":"{\"symbol\":\"eth/usd\"}""#),
            "Chainlink filters should be serialized as escaped JSON string, got: {json}"
        );
    }

    #[test]
    fn serialize_comments_subscription() {
        let sub = Subscription::comments(Some(CommentType::CommentCreated));
        let request = SubscriptionRequest::subscribe(vec![sub]);

        let json = serde_json::to_string(&request).unwrap();
        assert!(json.contains("\"topic\":\"comments\""));
        assert!(json.contains("\"type\":\"comment_created\""));
    }

    #[test]
    fn serialize_chainlink_without_filters() {
        // When no symbol is provided, there should be no filters field
        let sub = Subscription::chainlink_prices(None);
        let request = SubscriptionRequest::subscribe(vec![sub]);

        let json = serde_json::to_string(&request).unwrap();
        assert!(json.contains("\"topic\":\"crypto_prices_chainlink\""));
        assert!(!json.contains("\"filters\""));
    }

    #[test]
    fn serialize_crypto_prices_without_filters() {
        // When no symbols are provided, there should be no filters field
        let subs = Subscription::crypto_price_subscriptions(None);
        let request = SubscriptionRequest::subscribe(subs);

        let json = serde_json::to_string(&request).unwrap();
        assert!(json.contains("\"topic\":\"crypto_prices\""));
        assert!(!json.contains("\"filters\""));
    }

    #[test]
    fn serialize_mixed_subscriptions() {
        // Verify Chainlink and Binance subscriptions serialize differently in same request
        let chainlink = Subscription::chainlink_prices(Some("btc/usd".to_owned()));
        let mut subscriptions = vec![chainlink];
        subscriptions.extend(Subscription::crypto_price_subscriptions(Some(vec![
            "BTCUSDT".to_owned(),
            "ethusdt".to_owned(),
        ])));
        let request = SubscriptionRequest::subscribe(subscriptions);

        let json = serde_json::to_string(&request).unwrap();

        // Chainlink should have escaped string filters
        assert!(
            json.contains(r#""filters":"{\"symbol\":\"btc/usd\"}""#),
            "Chainlink filters should be escaped string, got: {json}"
        );
        // Binance should subscribe once per symbol using an escaped JSON string filter
        assert!(
            json.contains(r#""filters":"{\"symbol\":\"btcusdt\"}""#)
                && json.contains(r#""filters":"{\"symbol\":\"ethusdt\"}""#),
            "Binance filters should be one escaped JSON string per symbol, got: {json}"
        );
    }

    #[test]
    fn serialize_unsubscribe_request() {
        let sub = Subscription::crypto_prices(Some("btcusdt".to_owned()));
        let request = SubscriptionRequest::unsubscribe(vec![sub]);

        let json = serde_json::to_string(&request).unwrap();
        assert!(
            json.contains("\"action\":\"unsubscribe\""),
            "Action should be 'unsubscribe', got: {json}"
        );
        assert!(json.contains("\"topic\":\"crypto_prices\""));
        assert!(json.contains("\"type\":\"update\""));
    }

    #[test]
    fn serialize_unsubscribe_without_filters() {
        let sub = Subscription::crypto_prices(None);
        let request = SubscriptionRequest::unsubscribe(vec![sub]);

        let json = serde_json::to_string(&request).unwrap();
        assert!(json.contains("\"action\":\"unsubscribe\""));
        assert!(json.contains("\"topic\":\"crypto_prices\""));
        assert!(
            !json.contains("\"filters\""),
            "Should have no filters field"
        );
    }

    #[test]
    fn serialize_unsubscribe_chainlink() {
        let sub = Subscription::chainlink_prices(Some("btc/usd".to_owned()));
        let request = SubscriptionRequest::unsubscribe(vec![sub]);

        let json = serde_json::to_string(&request).unwrap();
        assert!(json.contains("\"action\":\"unsubscribe\""));
        assert!(json.contains("\"topic\":\"crypto_prices_chainlink\""));
        assert!(json.contains("\"type\":\"*\""));
    }

    #[test]
    fn serialize_unsubscribe_comments() {
        let sub = Subscription::comments(Some(CommentType::CommentCreated));
        let request = SubscriptionRequest::unsubscribe(vec![sub]);

        let json = serde_json::to_string(&request).unwrap();
        assert!(json.contains("\"action\":\"unsubscribe\""));
        assert!(json.contains("\"topic\":\"comments\""));
        assert!(json.contains("\"type\":\"comment_created\""));
    }

    #[test]
    fn serialize_unsubscribe_multiple_topics() {
        let crypto = Subscription::crypto_prices(None);
        let chainlink = Subscription::chainlink_prices(None);
        let comments = Subscription::comments(None);
        let request = SubscriptionRequest::unsubscribe(vec![crypto, chainlink, comments]);

        let json = serde_json::to_string(&request).unwrap();
        assert!(json.contains("\"action\":\"unsubscribe\""));
        assert!(json.contains("\"topic\":\"crypto_prices\""));
        assert!(json.contains("\"topic\":\"crypto_prices_chainlink\""));
        assert!(json.contains("\"topic\":\"comments\""));
    }

    #[test]
    fn serialize_equity_prices_update_subscription() {
        let sub = Subscription::equity_prices(Some("AAPL".to_owned()), "update");
        let request = SubscriptionRequest::subscribe(vec![sub]);

        let json = serde_json::to_string(&request).unwrap();
        assert!(json.contains("\"topic\":\"equity_prices\""));
        assert!(json.contains("\"type\":\"update\""));
        assert!(
            json.contains(r#""filters":"{\"symbol\":\"AAPL\"}""#),
            "Equity filters should be serialized as escaped JSON string, got: {json}"
        );
    }

    #[test]
    fn serialize_equity_prices_wildcard_subscription() {
        let sub = Subscription::equity_prices(Some("GOOGL".to_owned()), "*");
        let request = SubscriptionRequest::subscribe(vec![sub]);

        let json = serde_json::to_string(&request).unwrap();
        assert!(json.contains("\"topic\":\"equity_prices\""));
        assert!(json.contains("\"type\":\"*\""));
        assert!(
            json.contains(r#""filters":"{\"symbol\":\"GOOGL\"}""#),
            "Equity filters should be serialized as escaped JSON string, got: {json}"
        );
    }

    #[test]
    fn serialize_equity_prices_without_filters() {
        let sub = Subscription::equity_prices(None, "update");
        let request = SubscriptionRequest::subscribe(vec![sub]);

        let json = serde_json::to_string(&request).unwrap();
        assert!(json.contains("\"topic\":\"equity_prices\""));
        assert!(json.contains("\"type\":\"update\""));
        assert!(!json.contains("\"filters\""));
    }

    #[test]
    fn serialize_unsubscribe_equity_prices() {
        let sub = Subscription::equity_prices(Some("XAUUSD".to_owned()), "*");
        let request = SubscriptionRequest::unsubscribe(vec![sub]);

        let json = serde_json::to_string(&request).unwrap();
        assert!(json.contains("\"action\":\"unsubscribe\""));
        assert!(json.contains("\"topic\":\"equity_prices\""));
        assert!(json.contains("\"type\":\"*\""));
        assert!(
            json.contains(r#""filters":"{\"symbol\":\"XAUUSD\"}""#),
            "Equity filters should remain escaped on unsubscribe, got: {json}"
        );
    }
}
