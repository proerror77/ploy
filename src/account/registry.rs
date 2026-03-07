use crate::config::AccountConfig;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountRegistryEntry {
    pub account_id: String,
    pub wallet_address: Option<String>,
    pub label: Option<String>,
}

impl AccountRegistryEntry {
    pub fn normalize_account_id(raw: &str) -> String {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            "default".to_string()
        } else {
            trimmed.to_string()
        }
    }

    pub fn from_account_config(config: &AccountConfig) -> Self {
        Self {
            account_id: Self::normalize_account_id(&config.id),
            wallet_address: config
                .wallet_address
                .as_deref()
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(ToString::to_string),
            label: config
                .label
                .as_deref()
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(ToString::to_string),
        }
    }

    pub fn normalized(&self) -> Option<Self> {
        let account_id = Self::normalize_account_id(&self.account_id);
        if account_id.trim().is_empty() {
            return None;
        }

        Some(Self {
            account_id,
            wallet_address: self
                .wallet_address
                .as_deref()
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(ToString::to_string),
            label: self
                .label
                .as_deref()
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(ToString::to_string),
        })
    }
}
