mod config;
mod env;

pub use self::config::ManagedCryptoRuntimeConfig;

pub(super) fn apply_managed_crypto_runtime_env(
    crypto_cfg: &crate::strategy::CryptoTradingConfig,
    managed_cfg: &mut ManagedCryptoRuntimeConfig,
) {
    self::env::apply_managed_crypto_runtime_env(crypto_cfg, managed_cfg);
}
