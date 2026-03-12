use serde::Deserialize;

use crate::error::{PloyError, Result};

use super::blocks::{
    EntryBlockSpec, ExitBlockSpec, FilterBlockSpec, SignalBlockKind, SizingBlockSpec,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComposableCryptoSchema {
    pub signal_blocks: Vec<String>,
    pub filters: Vec<FilterBlockSpec>,
    pub entry: Vec<EntryBlockSpec>,
    pub exit: Vec<ExitBlockSpec>,
    pub sizing: Vec<SizingBlockSpec>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct RawComposableCryptoSchema {
    #[serde(default)]
    signal_blocks: Vec<String>,
    #[serde(default)]
    filters: Vec<FilterBlockSpec>,
    #[serde(default)]
    entry: Vec<EntryBlockSpec>,
    #[serde(default)]
    exit: Vec<ExitBlockSpec>,
    #[serde(default)]
    sizing: Vec<SizingBlockSpec>,
}

pub fn parse_composable_crypto_spec_toml(raw: &str) -> Result<ComposableCryptoSchema> {
    let parsed: RawComposableCryptoSchema = toml::from_str(raw).map_err(|err| {
        PloyError::Validation(format!("invalid composable crypto spec TOML: {err}"))
    })?;

    if parsed.signal_blocks.is_empty() {
        return Err(PloyError::Validation(
            "composable crypto spec requires at least one signal block".to_string(),
        ));
    }
    if parsed.entry.len() != 1 {
        return Err(PloyError::Validation(format!(
            "composable crypto spec requires exactly one entry block, got {}",
            parsed.entry.len()
        )));
    }
    if parsed.exit.len() != 1 {
        return Err(PloyError::Validation(format!(
            "composable crypto spec requires exactly one exit block, got {}",
            parsed.exit.len()
        )));
    }
    if parsed.sizing.len() != 1 {
        return Err(PloyError::Validation(format!(
            "composable crypto spec requires exactly one sizing block, got {}",
            parsed.sizing.len()
        )));
    }

    let mut signal_blocks = Vec::with_capacity(parsed.signal_blocks.len());
    for raw_signal in parsed.signal_blocks {
        let kind = raw_signal
            .parse::<SignalBlockKind>()
            .map_err(|_| PloyError::Validation(format!("unknown signal block: {raw_signal}")))?;
        signal_blocks.push(kind.as_str().to_string());
    }

    Ok(ComposableCryptoSchema {
        signal_blocks,
        filters: parsed.filters,
        entry: parsed.entry,
        exit: parsed.exit,
        sizing: parsed.sizing,
    })
}

#[cfg(test)]
mod tests {
    use super::parse_composable_crypto_spec_toml;

    #[test]
    fn valid_composable_crypto_spec_parses_with_one_block_per_stage() {
        let spec = parse_composable_crypto_spec_toml(
            r#"
signal_blocks = ["momentum"]

[[filters]]
type = "volatility_gate"

[[entry]]
type = "marketable_limit"

[[exit]]
type = "trailing_stop"

[[sizing]]
type = "fixed_shares"
"#,
        )
        .expect("valid composable crypto spec");

        assert_eq!(spec.signal_blocks, vec!["momentum".to_string()]);
        assert_eq!(spec.filters.len(), 1);
        assert_eq!(spec.entry.len(), 1);
        assert_eq!(spec.exit.len(), 1);
        assert_eq!(spec.sizing.len(), 1);
    }

    #[test]
    fn unknown_block_type_is_rejected() {
        let err = parse_composable_crypto_spec_toml(
            r#"
signal_blocks = ["surprise_alpha"]

[[entry]]
type = "marketable_limit"

[[exit]]
type = "trailing_stop"

[[sizing]]
type = "fixed_shares"
"#,
        )
        .expect_err("unknown signal block should fail");

        assert!(err.to_string().contains("unknown signal block"));
    }

    #[test]
    fn duplicate_singleton_sections_are_rejected() {
        let err = parse_composable_crypto_spec_toml(
            r#"
signal_blocks = ["momentum"]

[[entry]]
type = "marketable_limit"

[[entry]]
type = "ladder_limit"

[[exit]]
type = "trailing_stop"

[[sizing]]
type = "fixed_shares"
"#,
        )
        .expect_err("duplicate entry block should fail");

        assert!(err.to_string().contains("exactly one entry block"));
    }
}
