/// Input validation for external API data
///
/// This module provides validation functions for data received from external APIs
/// to prevent invalid data from causing incorrect trading decisions or system crashes.
///
/// # CRITICAL FIX
/// Previously, external API data was not validated, which could lead to:
/// - Trading on invalid prices (negative, zero, > 1.0)
/// - Using expired events
/// - Invalid share quantities
/// - Malformed timestamps
use crate::error::{PloyError, Result};
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;

/// Validate a binary option price (must be between 0 and 1)
///
/// # Arguments
/// * `price` - Price to validate
/// * `field_name` - Name of the field for error messages
///
/// # Returns
/// * `Ok(())` if valid
/// * `Err` if invalid
pub fn validate_price(price: Decimal, field_name: &str) -> Result<()> {
    if price < Decimal::ZERO {
        return Err(PloyError::Validation(format!(
            "{} cannot be negative: {}",
            field_name, price
        )));
    }

    if price > Decimal::ONE {
        return Err(PloyError::Validation(format!(
            "{} cannot be greater than 1.0: {}",
            field_name, price
        )));
    }

    Ok(())
}

/// Validate share quantity
///
/// # Arguments
/// * `shares` - Number of shares
/// * `max_shares` - Maximum allowed shares (default: 1,000,000)
///
/// # Returns
/// * `Ok(())` if valid
/// * `Err` if invalid
pub fn validate_shares(shares: u64, max_shares: Option<u64>) -> Result<()> {
    let max = max_shares.unwrap_or(1_000_000);

    if shares == 0 {
        return Err(PloyError::Validation("Shares cannot be zero".to_string()));
    }

    if shares > max {
        return Err(PloyError::Validation(format!(
            "Shares {} exceeds maximum {}",
            shares, max
        )));
    }

    Ok(())
}

/// Validate event end time (must be in the future)
///
/// # Arguments
/// * `end_time` - Event end time
/// * `min_time_remaining_secs` - Minimum time remaining in seconds (default: 60)
///
/// # Returns
/// * `Ok(())` if valid
/// * `Err` if invalid
pub fn validate_event_time(
    end_time: DateTime<Utc>,
    min_time_remaining_secs: Option<i64>,
) -> Result<()> {
    let now = Utc::now();
    let min_remaining = min_time_remaining_secs.unwrap_or(60);

    if end_time <= now {
        return Err(PloyError::Validation(format!(
            "Event has already ended at {}",
            end_time
        )));
    }

    let time_remaining = (end_time - now).num_seconds();
    if time_remaining < min_remaining {
        return Err(PloyError::Validation(format!(
            "Event ends too soon: {} seconds remaining (minimum: {})",
            time_remaining, min_remaining
        )));
    }

    Ok(())
}

/// Validate token ID format
///
/// # Arguments
/// * `token_id` - Token ID to validate
///
/// # Returns
/// * `Ok(())` if valid
/// * `Err` if invalid
pub fn validate_token_id(token_id: &str) -> Result<()> {
    if token_id.is_empty() {
        return Err(PloyError::Validation(
            "Token ID cannot be empty".to_string(),
        ));
    }

    // Token IDs should be hex strings (with or without 0x prefix)
    let hex_part = token_id.trim_start_matches("0x");
    if hex_part.is_empty() || !hex_part.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(PloyError::Validation(format!(
            "Invalid token ID format: {}",
            token_id
        )));
    }

    Ok(())
}

/// Validate market data from external API
///
/// # Arguments
/// * `best_bid` - Best bid price (optional)
/// * `best_ask` - Best ask price (optional)
///
/// # Returns
/// * `Ok(())` if valid
/// * `Err` if invalid
pub fn validate_market_data(best_bid: Option<Decimal>, best_ask: Option<Decimal>) -> Result<()> {
    if let Some(bid) = best_bid {
        validate_price(bid, "best_bid")?;
    }

    if let Some(ask) = best_ask {
        validate_price(ask, "best_ask")?;
    }

    // If both exist, bid should be <= ask
    if let (Some(bid), Some(ask)) = (best_bid, best_ask) {
        if bid > ask {
            return Err(PloyError::Validation(format!(
                "Best bid ({}) cannot be greater than best ask ({})",
                bid, ask
            )));
        }
    }

    Ok(())
}

/// Validate event data from external API
///
/// # Arguments
/// * `event_id` - Event ID
/// * `end_time` - Event end time
/// * `up_token_id` - UP token ID
/// * `down_token_id` - DOWN token ID
///
/// # Returns
/// * `Ok(())` if valid
/// * `Err` if invalid
pub fn validate_event_data(
    event_id: &str,
    end_time: DateTime<Utc>,
    up_token_id: &str,
    down_token_id: &str,
) -> Result<()> {
    // Validate event ID
    if event_id.is_empty() {
        return Err(PloyError::Validation(
            "Event ID cannot be empty".to_string(),
        ));
    }

    // Validate end time
    validate_event_time(end_time, Some(60))?;

    // Validate token IDs
    validate_token_id(up_token_id)?;
    validate_token_id(down_token_id)?;

    // Token IDs should be different
    if up_token_id == down_token_id {
        return Err(PloyError::Validation(
            "UP and DOWN token IDs must be different".to_string(),
        ));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn test_validate_price() {
        // Valid prices
        assert!(validate_price(dec!(0.5), "price").is_ok());
        assert!(validate_price(dec!(0.0), "price").is_ok());
        assert!(validate_price(dec!(1.0), "price").is_ok());

        // Invalid prices
        assert!(validate_price(dec!(-0.1), "price").is_err());
        assert!(validate_price(dec!(1.1), "price").is_err());
    }

    #[test]
    fn test_validate_shares() {
        // Valid shares
        assert!(validate_shares(1, None).is_ok());
        assert!(validate_shares(1000, None).is_ok());

        // Invalid shares
        assert!(validate_shares(0, None).is_err());
        assert!(validate_shares(2_000_000, None).is_err());
        assert!(validate_shares(100, Some(50)).is_err());
    }

    #[test]
    fn test_validate_event_time() {
        // Valid time (1 hour in future)
        let future = Utc::now() + chrono::Duration::hours(1);
        assert!(validate_event_time(future, Some(60)).is_ok());

        // Invalid time (past)
        let past = Utc::now() - chrono::Duration::hours(1);
        assert!(validate_event_time(past, Some(60)).is_err());

        // Invalid time (too soon)
        let too_soon = Utc::now() + chrono::Duration::seconds(30);
        assert!(validate_event_time(too_soon, Some(60)).is_err());
    }

    #[test]
    fn test_validate_token_id() {
        // Valid token IDs
        assert!(validate_token_id("0x1234567890abcdef").is_ok());
        assert!(validate_token_id("1234567890abcdef").is_ok());

        // Invalid token IDs
        assert!(validate_token_id("").is_err());
        assert!(validate_token_id("0x").is_err());
        assert!(validate_token_id("not_hex").is_err());
    }

    #[test]
    fn test_validate_market_data() {
        // Valid market data
        assert!(validate_market_data(Some(dec!(0.4)), Some(dec!(0.6))).is_ok());
        assert!(validate_market_data(Some(dec!(0.5)), Some(dec!(0.5))).is_ok());
        assert!(validate_market_data(None, Some(dec!(0.5))).is_ok());

        // Invalid market data
        assert!(validate_market_data(Some(dec!(0.6)), Some(dec!(0.4))).is_err());
        assert!(validate_market_data(Some(dec!(-0.1)), Some(dec!(0.5))).is_err());
        assert!(validate_market_data(Some(dec!(0.5)), Some(dec!(1.5))).is_err());
    }
}
