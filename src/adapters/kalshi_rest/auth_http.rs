//! Kalshi auth/signing and shared HTTP request plumbing.

use super::KalshiClient;
use crate::error::{PloyError, Result};
use base64::{Engine, engine::general_purpose::STANDARD as BASE64_STANDARD};
use chrono::Utc;
use hmac::{Hmac, Mac};
use reqwest::header::{CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue};
use reqwest::{Client, Method};
use serde_json::Value;
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

impl KalshiClient {
    pub(super) fn build_http_client() -> Result<Client> {
        Client::builder()
            .user_agent("ploy-kalshi-adapter/0.1")
            .build()
            .map_err(|e| PloyError::Internal(format!("failed to build Kalshi HTTP client: {}", e)))
    }

    fn auth_headers(&self, method: &Method, path: &str, body: &str) -> Result<HeaderMap> {
        let key = self.api_key.as_ref().ok_or_else(|| {
            PloyError::Auth("KALSHI_API_KEY (or KALSHI_ACCESS_KEY) is required".to_string())
        })?;
        let secret = self.api_secret.as_ref().ok_or_else(|| {
            PloyError::Auth("KALSHI_API_SECRET (or KALSHI_ACCESS_SECRET) is required".to_string())
        })?;

        let timestamp = Utc::now().timestamp_millis().to_string();
        let sign_payload = Self::build_sign_payload(&timestamp, method, path, body);
        let signature = Self::hmac_signature(secret, &sign_payload)?;

        let mut headers = HeaderMap::new();
        headers.insert(
            HeaderName::from_static("kalshi-access-key"),
            HeaderValue::from_str(key)
                .map_err(|e| PloyError::Auth(format!("invalid Kalshi API key header: {}", e)))?,
        );
        headers.insert(
            HeaderName::from_static("kalshi-access-signature"),
            HeaderValue::from_str(&signature)
                .map_err(|e| PloyError::Auth(format!("invalid Kalshi signature header: {}", e)))?,
        );
        headers.insert(
            HeaderName::from_static("kalshi-access-timestamp"),
            HeaderValue::from_str(&timestamp)
                .map_err(|e| PloyError::Auth(format!("invalid Kalshi timestamp header: {}", e)))?,
        );

        Ok(headers)
    }

    pub(super) async fn request_json(
        &self,
        method: Method,
        path: &str,
        query: Option<&[(&str, String)]>,
        body: Option<Value>,
        require_auth: bool,
    ) -> Result<Value> {
        let url = format!("{}{}", self.base_url, path);
        let body_text = body
            .as_ref()
            .map(|value| value.to_string())
            .unwrap_or_default();

        let mut req = self.http.request(method.clone(), &url);

        if let Some(query) = query {
            req = req.query(query);
        }

        if require_auth {
            req = req.headers(self.auth_headers(&method, path, &body_text)?);
        }

        if let Some(body) = body {
            req = req.header(CONTENT_TYPE, "application/json").json(&body);
        }

        let resp = req.send().await?;
        let status = resp.status();
        let text = resp.text().await?;

        if status.as_u16() == 429 {
            return Err(PloyError::RateLimited(format!(
                "Kalshi API rate limited for {} {}",
                method, path
            )));
        }

        if !status.is_success() {
            return Err(PloyError::Internal(format!(
                "Kalshi API {} {} failed: status={} body={}",
                method, path, status, text
            )));
        }

        if text.trim().is_empty() {
            return Ok(Value::Null);
        }

        serde_json::from_str(&text)
            .map_err(|e| PloyError::Internal(format!("invalid Kalshi JSON response: {}", e)))
    }

    pub(super) fn build_sign_payload(
        timestamp: &str,
        method: &Method,
        path: &str,
        body: &str,
    ) -> String {
        format!(
            "{}{}{}{}",
            timestamp,
            method.as_str().to_uppercase(),
            path,
            body
        )
    }

    pub(super) fn hmac_signature(secret: &str, payload: &str) -> Result<String> {
        let mut mac = HmacSha256::new_from_slice(secret.as_bytes())
            .map_err(|e| PloyError::Auth(format!("invalid Kalshi secret: {}", e)))?;
        mac.update(payload.as_bytes());
        Ok(BASE64_STANDARD.encode(mac.finalize().into_bytes()))
    }
}
