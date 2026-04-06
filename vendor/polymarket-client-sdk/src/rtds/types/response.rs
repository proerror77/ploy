use bon::Builder;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::types::{Address, Decimal};

/// Top-level RTDS message wrapper.
///
/// All messages received from the RTDS WebSocket connection are deserialized into this struct.
#[non_exhaustive]
#[derive(Debug, Clone, Deserialize, Builder)]
pub struct RtdsMessage {
    /// The subscription topic (e.g., `crypto_prices`, `comments`)
    pub topic: String,
    /// The message type/event (e.g., `update`, `comment_created`)
    #[serde(rename = "type")]
    pub msg_type: String,
    /// Unix timestamp in milliseconds
    pub timestamp: i64,
    /// Event-specific data object
    pub payload: Value,
}

impl RtdsMessage {
    /// Try to extract the payload as a crypto price update.
    #[must_use]
    pub fn as_crypto_price(&self) -> Option<CryptoPrice> {
        if self.topic == "crypto_prices" {
            serde_json::from_value(self.payload.clone()).ok()
        } else {
            None
        }
    }

    /// Try to extract the payload as a Chainlink price update.
    #[must_use]
    pub fn as_chainlink_price(&self) -> Option<ChainlinkPrice> {
        if self.topic == "crypto_prices_chainlink" {
            serde_json::from_value(self.payload.clone()).ok()
        } else {
            None
        }
    }

    /// Try to extract the payload as an equity price live update.
    #[must_use]
    pub fn as_equity_price_update(&self) -> Option<EquityPriceUpdate> {
        if self.topic == "equity_prices" && self.msg_type == "update" {
            serde_json::from_value(self.payload.clone()).ok()
        } else {
            None
        }
    }

    /// Try to extract the payload as an equity price snapshot/backfill message.
    #[must_use]
    pub fn as_equity_price_snapshot(&self) -> Option<EquityPriceSnapshot> {
        if self.topic == "equity_prices" && self.msg_type == "subscribe" {
            serde_json::from_value(self.payload.clone()).ok()
        } else {
            None
        }
    }

    /// Try to extract the payload as a comment event.
    #[must_use]
    pub fn as_comment(&self) -> Option<Comment> {
        if self.topic == "comments" {
            serde_json::from_value(self.payload.clone()).ok()
        } else {
            None
        }
    }
}

/// Binance crypto price update payload.
#[non_exhaustive]
#[derive(Debug, Clone, Deserialize, Serialize, Builder)]
pub struct CryptoPrice {
    /// Trading pair symbol (lowercase concatenated, e.g., "solusdt", "btcusdt")
    pub symbol: String,
    /// Price timestamp in Unix milliseconds
    pub timestamp: i64,
    /// Current price value
    pub value: Decimal,
}

/// Chainlink price feed update payload.
#[non_exhaustive]
#[derive(Debug, Clone, Deserialize, Serialize, Builder)]
pub struct ChainlinkPrice {
    /// Trading pair symbol (slash-separated, e.g., "eth/usd", "btc/usd")
    pub symbol: String,
    /// Price timestamp in Unix milliseconds
    pub timestamp: i64,
    /// Current price value
    pub value: Decimal,
}

/// Equity and commodity price update payload.
#[non_exhaustive]
#[derive(Debug, Clone, Deserialize, Serialize, Builder)]
pub struct EquityPriceUpdate {
    /// Lowercase symbol identifier (e.g. `aapl`, `xauusd`, `wti`)
    pub symbol: String,
    /// Current price value
    pub value: Decimal,
    /// Full-precision price string emitted by RTDS
    pub full_accuracy_value: String,
    /// Price timestamp in Unix milliseconds
    pub timestamp: i64,
    /// When the RTDS pipeline received the price
    #[serde(default)]
    pub received_at: Option<i64>,
    /// Whether the price is carried forward outside market hours
    #[serde(default)]
    pub is_carried_forward: bool,
}

/// One point inside an equity price snapshot/backfill payload.
#[non_exhaustive]
#[derive(Debug, Clone, Deserialize, Serialize, Builder)]
pub struct EquityPriceSnapshotPoint {
    /// Price timestamp in Unix milliseconds
    pub timestamp: i64,
    /// Point-in-time value
    pub value: Decimal,
    /// Optional full-precision value string if RTDS emits it
    #[serde(default)]
    pub full_accuracy_value: Option<String>,
    /// Optional receive timestamp if RTDS emits it
    #[serde(default)]
    pub received_at: Option<i64>,
    /// Whether the point is carried forward
    #[serde(default)]
    pub is_carried_forward: bool,
}

/// Historical snapshot payload delivered on subscription.
#[non_exhaustive]
#[derive(Debug, Clone, Deserialize, Serialize, Builder)]
pub struct EquityPriceSnapshot {
    /// Lowercase symbol identifier (e.g. `aapl`)
    pub symbol: String,
    /// Last 2 minutes of data returned by RTDS on subscribe
    pub data: Vec<EquityPriceSnapshotPoint>,
}

/// Comment event payload.
#[non_exhaustive]
#[derive(Debug, Clone, Deserialize, Serialize, Builder)]
pub struct Comment {
    /// Unique identifier for this comment
    pub id: String,
    /// The text content of the comment
    pub body: String,
    /// ISO 8601 timestamp when the comment was created
    #[serde(rename = "createdAt")]
    pub created_at: DateTime<Utc>,
    /// ID of the parent comment if this is a reply (null for top-level comments)
    #[serde(rename = "parentCommentID", default)]
    pub parent_comment_id: Option<String>,
    /// ID of the parent entity (event, market, etc.)
    #[serde(rename = "parentEntityID")]
    pub parent_entity_id: i64,
    /// Type of parent entity (e.g., "Event", "Market")
    #[serde(rename = "parentEntityType")]
    pub parent_entity_type: String,
    /// Profile information of the user who created the comment
    pub profile: CommentProfile,
    /// Current number of reactions on this comment
    #[serde(rename = "reactionCount", default)]
    pub reaction_count: i64,
    /// Polygon address for replies
    #[serde(rename = "replyAddress", default)]
    pub reply_address: Option<Address>,
    /// Current number of reports on this comment
    #[serde(rename = "reportCount", default)]
    pub report_count: i64,
    /// Polygon address of the user who created the comment
    #[serde(rename = "userAddress")]
    pub user_address: Address,
}

/// Profile information for a comment author.
#[non_exhaustive]
#[derive(Debug, Clone, Deserialize, Serialize, Builder)]
pub struct CommentProfile {
    /// User profile address
    #[serde(rename = "baseAddress")]
    pub base_address: Address,
    /// Whether the username should be displayed publicly
    #[serde(rename = "displayUsernamePublic", default)]
    pub display_username_public: bool,
    /// User's display name
    pub name: String,
    /// Proxy wallet address used for transactions
    #[serde(rename = "proxyWallet", default)]
    pub proxy_wallet: Option<Address>,
    /// Generated pseudonym for the user
    #[serde(default)]
    pub pseudonym: Option<String>,
}

/// Comment message types.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CommentType {
    /// New comment created
    CommentCreated,
    /// Comment was removed/deleted
    CommentRemoved,
    /// Reaction added to a comment
    ReactionCreated,
    /// Reaction removed from a comment
    ReactionRemoved,
    /// Unknown comment type from the API (captures the raw value for debugging).
    #[serde(untagged)]
    Unknown(String),
}

fn is_error_envelope(map: &Map<String, Value>) -> bool {
    !map.contains_key("topic")
        && map.contains_key("statusCode")
        && map
            .get("body")
            .is_some_and(|body| matches!(body, Value::Object(_)))
}

fn parse_message_value(value: Value) -> crate::Result<Option<RtdsMessage>> {
    match value {
        Value::Object(map) if is_error_envelope(&map) => Ok(None),
        other => Ok(Some(serde_json::from_value(other)?)),
    }
}

/// Deserialize messages from the byte slice.
///
/// Handles both single objects and arrays of messages.
/// Returns an empty vector for empty or whitespace-only input.
pub fn parse_messages(bytes: &[u8]) -> crate::Result<Vec<RtdsMessage>> {
    // Handle empty or whitespace-only input (server keepalive messages)
    let trimmed = bytes
        .iter()
        .position(|b| !b.is_ascii_whitespace())
        .map_or(&[][..], |start| &bytes[start..]);

    if trimmed.is_empty() {
        return Ok(Vec::new());
    }

    let parsed: Value = serde_json::from_slice(trimmed)?;

    match parsed {
        Value::Array(values) => {
            let mut messages = Vec::with_capacity(values.len());
            for value in values {
                if let Some(message) = parse_message_value(value)? {
                    messages.push(message);
                }
            }
            Ok(messages)
        }
        other => Ok(parse_message_value(other)?.into_iter().collect()),
    }
}

#[cfg(test)]
mod tests {
    use rust_decimal_macros::dec;

    use super::*;

    #[test]
    fn parse_crypto_price_message() {
        let json = r#"{
            "topic": "crypto_prices",
            "type": "update",
            "timestamp": 1753314064237,
            "payload": {
                "symbol": "solusdt",
                "timestamp": 1753314064213,
                "value": 189.55
            }
        }"#;

        let msgs = parse_messages(json.as_bytes()).unwrap();
        assert_eq!(msgs.len(), 1);

        let msg = &msgs[0];
        assert_eq!(msg.topic, "crypto_prices");
        assert_eq!(msg.msg_type, "update");

        let price = msg.as_crypto_price().unwrap();
        assert_eq!(price.symbol, "solusdt");
        assert_eq!(price.value, dec!(189.55));
    }

    #[test]
    fn parse_chainlink_price_message() {
        let json = r#"{
            "topic": "crypto_prices_chainlink",
            "type": "update",
            "timestamp": 1753314064237,
            "payload": {
                "symbol": "eth/usd",
                "timestamp": 1753314064213,
                "value": 3456.78
            }
        }"#;

        let msgs = parse_messages(json.as_bytes()).unwrap();
        assert_eq!(msgs.len(), 1);

        let msg = &msgs[0];
        assert_eq!(msg.topic, "crypto_prices_chainlink");

        let price = msg.as_chainlink_price().unwrap();
        assert_eq!(price.symbol, "eth/usd");
        assert_eq!(price.value, dec!(3456.78));
    }

    #[test]
    fn parse_comment_message() {
        let json = r#"{
            "topic": "comments",
            "type": "comment_created",
            "timestamp": 1753454975808,
            "payload": {
                "body": "Test comment",
                "createdAt": "2025-07-25T14:49:35.801298Z",
                "id": "1763355",
                "parentCommentID": "1763325",
                "parentEntityID": 18396,
                "parentEntityType": "Event",
                "profile": {
                    "baseAddress": "0xce533188d53a16ed580fd5121dedf166d3482677",
                    "displayUsernamePublic": true,
                    "name": "salted.caramel",
                    "proxyWallet": "0x4ca749dcfa93c87e5ee23e2d21ff4422c7a4c1ee",
                    "pseudonym": "Adored-Disparity"
                },
                "reactionCount": 0,
                "replyAddress": "0x0bda5d16f76cd1d3485bcc7a44bc6fa7db004cdd",
                "reportCount": 0,
                "userAddress": "0xce533188d53a16ed580fd5121dedf166d3482677"
            }
        }"#;

        let msgs = parse_messages(json.as_bytes()).unwrap();
        assert_eq!(msgs.len(), 1);

        let msg = &msgs[0];
        assert_eq!(msg.topic, "comments");
        assert_eq!(msg.msg_type, "comment_created");

        let comment = msg.as_comment().unwrap();
        assert_eq!(comment.id, "1763355");
        assert_eq!(comment.body, "Test comment");
        assert_eq!(comment.profile.name, "salted.caramel");
    }

    #[test]
    fn parse_message_array() {
        let json = r#"[{
            "topic": "crypto_prices",
            "type": "update",
            "timestamp": 1753314064237,
            "payload": {
                "symbol": "btcusdt",
                "timestamp": 1753314064213,
                "value": 67234.50
            }
        }]"#;

        let msgs = parse_messages(json.as_bytes()).unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].topic, "crypto_prices");
    }

    #[test]
    fn parse_empty_input() {
        let msgs = parse_messages(b"").unwrap();
        assert!(msgs.is_empty());
    }

    #[test]
    fn parse_whitespace_only_input() {
        let msgs = parse_messages(b"   \n\t  ").unwrap();
        assert!(msgs.is_empty());
    }

    #[test]
    fn ignore_error_envelope_without_topic() {
        let json = r#"{
            "body": {
                "message": "invalid Subscriptions.Subscriptions[0]: embedded message failed validation"
            },
            "statusCode": 400
        }"#;

        let msgs = parse_messages(json.as_bytes()).unwrap();
        assert!(msgs.is_empty());
    }

    #[test]
    fn parse_equity_price_update_message() {
        let json = r#"{
            "topic": "equity_prices",
            "type": "update",
            "timestamp": 1711382400000,
            "payload": {
                "symbol": "aapl",
                "value": 198.45,
                "full_accuracy_value": "198.4523",
                "timestamp": 1711382400000,
                "received_at": 1711382400005
            }
        }"#;

        let msgs = parse_messages(json.as_bytes()).unwrap();
        assert_eq!(msgs.len(), 1);

        let msg = &msgs[0];
        assert_eq!(msg.topic, "equity_prices");
        assert_eq!(msg.msg_type, "update");

        let update = msg.as_equity_price_update().unwrap();
        assert_eq!(update.symbol, "aapl");
        assert_eq!(update.value, dec!(198.45));
        assert_eq!(update.full_accuracy_value, "198.4523");
        assert_eq!(update.timestamp, 1_711_382_400_000);
        assert_eq!(update.received_at, Some(1_711_382_400_005));
        assert!(!update.is_carried_forward);
    }

    #[test]
    fn parse_equity_price_update_with_carried_forward_flag() {
        let json = r#"{
            "topic": "equity_prices",
            "type": "update",
            "timestamp": 1711400000000,
            "payload": {
                "symbol": "xauusd",
                "value": 2175.30,
                "full_accuracy_value": "2175.3012",
                "timestamp": 1711399000000,
                "received_at": 1711400000002,
                "is_carried_forward": true
            }
        }"#;

        let msgs = parse_messages(json.as_bytes()).unwrap();
        let update = msgs[0].as_equity_price_update().unwrap();
        assert_eq!(update.symbol, "xauusd");
        assert_eq!(update.value, dec!(2175.30));
        assert_eq!(update.full_accuracy_value, "2175.3012");
        assert!(update.is_carried_forward);
    }

    #[test]
    fn parse_equity_price_snapshot_message() {
        let json = r#"{
            "topic": "equity_prices",
            "type": "subscribe",
            "timestamp": 1711382400000,
            "payload": {
                "symbol": "aapl",
                "data": [
                    { "timestamp": 1711382280000, "value": 198.30 },
                    { "timestamp": 1711382281000, "value": 198.32 },
                    { "timestamp": 1711382340000, "value": 198.41 }
                ]
            }
        }"#;

        let msgs = parse_messages(json.as_bytes()).unwrap();
        assert_eq!(msgs.len(), 1);

        let msg = &msgs[0];
        assert_eq!(msg.topic, "equity_prices");
        assert_eq!(msg.msg_type, "subscribe");

        let snapshot = msg.as_equity_price_snapshot().unwrap();
        assert_eq!(snapshot.symbol, "aapl");
        assert_eq!(snapshot.data.len(), 3);
        assert_eq!(snapshot.data[0].timestamp, 1_711_382_280_000);
        assert_eq!(snapshot.data[0].value, dec!(198.30));
        assert_eq!(snapshot.data[2].value, dec!(198.41));
    }
}
