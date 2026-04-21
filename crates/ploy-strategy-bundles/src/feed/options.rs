/// Additive historical-loader flags for non-crypto datasets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoricalLoadOptions {
    pub include_reference_prices: bool,
    pub reference_symbols: Vec<String>,
    pub include_sports_state: bool,
    pub require_official_settlement: bool,
    /// Downsample `binance_lob_ticks` to one snapshot per N seconds per symbol.
    /// Defaults to 30 (one row per 30-second bucket). Set to 1 to disable downsampling.
    pub lob_sample_secs: u32,
}

impl Default for HistoricalLoadOptions {
    fn default() -> Self {
        Self {
            include_reference_prices: false,
            reference_symbols: Vec::new(),
            include_sports_state: false,
            require_official_settlement: false,
            lob_sample_secs: 30,
        }
    }
}

impl HistoricalLoadOptions {
    #[must_use]
    pub fn normalized_reference_symbols(&self) -> Vec<String> {
        self.reference_symbols
            .iter()
            .map(|symbol| symbol.trim().to_lowercase())
            .filter(|symbol| !symbol.is_empty())
            .collect()
    }
}

#[must_use]
#[cfg(any(feature = "parquet-feed", test))]
pub fn normalize_token_id(raw: &str) -> String {
    let value = raw.trim().trim_matches('"');
    if let Some(hex) = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
    {
        return hex_to_decimal_string(hex).unwrap_or_else(|| value.to_string());
    }
    value.to_string()
}

#[cfg(any(feature = "parquet-feed", test))]
fn hex_to_decimal_string(hex: &str) -> Option<String> {
    if hex.is_empty() {
        return None;
    }

    let mut digits = vec![0_u8];

    for ch in hex.chars() {
        let value = ch.to_digit(16)? as u32;
        let mut carry = value;

        for digit in &mut digits {
            let next = (*digit as u32) * 16 + carry;
            *digit = (next % 10) as u8;
            carry = next / 10;
        }

        while carry > 0 {
            digits.push((carry % 10) as u8);
            carry /= 10;
        }
    }

    while digits.len() > 1 && digits.last() == Some(&0) {
        digits.pop();
    }

    Some(
        digits
            .iter()
            .rev()
            .map(|digit| char::from(b'0' + *digit))
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::normalize_token_id;

    #[test]
    fn normalize_token_id_converts_large_hex() {
        let raw = "\"0x3c38c18444ab803acea0d4de7bcdecae7f0f8ddbcd0466e3323d1cb9e04b6f5d\"";
        assert_eq!(
            normalize_token_id(raw),
            "27239049953613250678046988034203198692578441444398010699401021233149338414941"
        );
    }

    #[test]
    fn normalize_token_id_keeps_decimal_ids() {
        let raw = "12345678901234567890";
        assert_eq!(normalize_token_id(raw), raw);
    }
}
