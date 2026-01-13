use crate::error::{PloyError, Result};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use hmac::{Hmac, Mac};
use reqwest::header::{HeaderMap, HeaderValue};
use sha2::Sha256;
use std::time::{SystemTime, UNIX_EPOCH};

type HmacSha256 = Hmac<Sha256>;

/// API credentials for L2 authentication
#[derive(Debug, Clone)]
pub struct ApiCredentials {
    pub api_key: String,
    pub secret: String,
    pub passphrase: String,
}

impl ApiCredentials {
    pub fn new(api_key: String, secret: String, passphrase: String) -> Self {
        Self {
            api_key,
            secret,
            passphrase,
        }
    }

    /// Load from environment variables
    pub fn from_env() -> Result<Self> {
        let api_key = std::env::var("POLYMARKET_API_KEY")
            .map_err(|_| PloyError::Config(config::ConfigError::NotFound("POLYMARKET_API_KEY".into())))?;
        let secret = std::env::var("POLYMARKET_SECRET")
            .map_err(|_| PloyError::Config(config::ConfigError::NotFound("POLYMARKET_SECRET".into())))?;
        let passphrase = std::env::var("POLYMARKET_PASSPHRASE")
            .map_err(|_| PloyError::Config(config::ConfigError::NotFound("POLYMARKET_PASSPHRASE".into())))?;

        Ok(Self::new(api_key, secret, passphrase))
    }
}

/// HMAC authentication helper for L2 API requests
#[derive(Clone)]
pub struct HmacAuth {
    credentials: ApiCredentials,
    address: String,
}

impl HmacAuth {
    pub fn new(credentials: ApiCredentials, address: String) -> Self {
        Self {
            credentials,
            address,
        }
    }

    /// Get current timestamp in seconds
    fn timestamp() -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("System clock is before UNIX epoch")
            .as_secs() as i64
    }

    /// Create HMAC-SHA256 signature
    fn sign(&self, message: &str) -> Result<String> {
        let secret_bytes = BASE64
            .decode(&self.credentials.secret)
            .map_err(|e| PloyError::Signature(format!("Invalid secret encoding: {}", e)))?;

        let mut mac = HmacSha256::new_from_slice(&secret_bytes)
            .map_err(|e| PloyError::Signature(format!("HMAC init failed: {}", e)))?;

        mac.update(message.as_bytes());
        let result = mac.finalize();

        Ok(BASE64.encode(result.into_bytes()))
    }

    /// Build the message to sign for a request
    fn build_message(
        &self,
        method: &str,
        path: &str,
        timestamp: i64,
        body: Option<&str>,
    ) -> String {
        match body {
            Some(b) if !b.is_empty() => {
                format!("{}{}{}{}", timestamp, method.to_uppercase(), path, b)
            }
            _ => format!("{}{}{}", timestamp, method.to_uppercase(), path),
        }
    }

    /// Build authentication headers for a request
    pub fn build_headers(
        &self,
        method: &str,
        path: &str,
        body: Option<&str>,
    ) -> Result<HeaderMap> {
        let timestamp = Self::timestamp();
        let message = self.build_message(method, path, timestamp, body);
        let signature = self.sign(&message)?;

        // Debug: Log what we're signing
        tracing::debug!(
            "HMAC signing - timestamp: {}, method: {}, path: {}, message: '{}', address: {}",
            timestamp, method, path, message, self.address
        );

        let mut headers = HeaderMap::new();

        headers.insert(
            "POLY_ADDRESS",
            HeaderValue::from_str(&self.address)
                .map_err(|e| PloyError::Internal(format!("Invalid address header: {}", e)))?,
        );
        headers.insert(
            "POLY_SIGNATURE",
            HeaderValue::from_str(&signature)
                .map_err(|e| PloyError::Internal(format!("Invalid signature header: {}", e)))?,
        );
        headers.insert(
            "POLY_TIMESTAMP",
            HeaderValue::from_str(&timestamp.to_string())
                .map_err(|e| PloyError::Internal(format!("Invalid timestamp header: {}", e)))?,
        );
        headers.insert(
            "POLY_API_KEY",
            HeaderValue::from_str(&self.credentials.api_key)
                .map_err(|e| PloyError::Internal(format!("Invalid API key header: {}", e)))?,
        );
        headers.insert(
            "POLY_PASSPHRASE",
            HeaderValue::from_str(&self.credentials.passphrase)
                .map_err(|e| PloyError::Internal(format!("Invalid passphrase header: {}", e)))?,
        );

        Ok(headers)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_message() {
        let creds = ApiCredentials::new(
            "test-key".to_string(),
            BASE64.encode(b"test-secret"),
            "test-pass".to_string(),
        );
        let auth = HmacAuth::new(creds, "0x1234".to_string());

        let msg = auth.build_message("POST", "/order", 1704067200, Some(r#"{"test":"data"}"#));
        assert_eq!(msg, r#"1704067200POST/order{"test":"data"}"#);

        let msg_no_body = auth.build_message("GET", "/orders", 1704067200, None);
        assert_eq!(msg_no_body, "1704067200GET/orders");
    }

    #[test]
    fn test_sign() {
        let creds = ApiCredentials::new(
            "test-key".to_string(),
            BASE64.encode(b"test-secret"),
            "test-pass".to_string(),
        );
        let auth = HmacAuth::new(creds, "0x1234".to_string());

        let sig = auth.sign("test message").unwrap();
        // Should produce a base64 encoded signature
        assert!(!sig.is_empty());
        assert!(BASE64.decode(&sig).is_ok());
    }
}
