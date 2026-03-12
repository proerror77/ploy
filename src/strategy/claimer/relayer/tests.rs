use super::proxy_support::{ensure_0x_prefix, relayer_hmac_signature};
use super::*;

use alloy::primitives::{Address, U256};
use alloy::signers::{local::PrivateKeySigner, Signer as _};
use std::sync::Mutex;

static ENV_LOCK: Mutex<()> = Mutex::new(());

fn set_env(key: &str, value: Option<&str>) {
    match value {
        Some(v) => std::env::set_var(key, v),
        None => std::env::remove_var(key),
    }
}

#[test]
fn test_relayer_hmac_signature_urlsafe_base64() {
    let sig = relayer_hmac_signature("dGVzdHNlY3JldA==", "123POST/submit{\"a\":1}")
        .expect("signature should be created");
    assert_eq!(sig, "5UKMaApqgL6X7RdBVDJLKCU_aDY7kSpONfbGIEZAX0s=");
}

#[test]
fn test_relayer_hmac_signature_accepts_urlsafe_secret_variant() {
    let sig = relayer_hmac_signature(
        "Ndt7ZPLgVWpSzXHGFMohLB33x_Z4qCfqjiMYBwmxamE=",
        "1700000000POST/submit{}",
    )
    .expect("url-safe builder secret should decode");
    assert!(!sig.is_empty());
}

#[test]
fn test_ensure_0x_prefix() {
    assert_eq!(ensure_0x_prefix("abcd"), "0xabcd");
    assert_eq!(ensure_0x_prefix("0xabcd"), "0xabcd");
}

#[test]
fn test_missing_relayer_builder_credential_groups() {
    let _guard = ENV_LOCK.lock().expect("env lock");

    let all_keys: Vec<&str> = RELAYER_BUILDER_API_KEY_ENV_KEYS
        .iter()
        .chain(RELAYER_BUILDER_SECRET_ENV_KEYS.iter())
        .chain(RELAYER_BUILDER_PASSPHRASE_ENV_KEYS.iter())
        .copied()
        .collect();
    let prev: Vec<(&str, Option<String>)> = all_keys
        .iter()
        .map(|k| (*k, std::env::var(k).ok()))
        .collect();

    for key in &all_keys {
        set_env(key, None);
    }
    set_env("POLY_BUILDER_API_KEY", Some("k"));
    set_env("POLY_BUILDER_PASSPHRASE", Some("p"));
    assert_eq!(missing_relayer_builder_credential_groups(), vec!["secret"]);

    set_env("BUILDER_SECRET", Some("s"));
    assert!(missing_relayer_builder_credential_groups().is_empty());

    for (key, val) in prev {
        set_env(key, val.as_deref());
    }
}

#[test]
fn test_derive_proxy_wallet_address_matches_known_vector() {
    let signer: Address = "0x9d699747148fd637a7d2514f9b3e3028bf59195c"
        .parse()
        .expect("valid signer");
    let proxy = AutoClaimer::derive_proxy_wallet_address(signer)
        .expect("proxy address should derive correctly");
    assert_eq!(
        format!("{:#x}", proxy),
        "0xcbaaa60c5dec85eac2a2c424bdcd7258ab67eee2"
    );
}

#[test]
fn test_encode_proxy_transaction_data_accepts_tuple_calls() {
    let call_to: Address = "0x4D97DCd97eC945f40cF65F87097ACe5EA0476045"
        .parse()
        .expect("valid call target");
    let encoded = AutoClaimer::encode_proxy_transaction_data(call_to, vec![0x12, 0x34])
        .expect("proxy calldata should encode");
    assert!(!encoded.is_empty());
}

#[tokio::test]
async fn test_proxy_signature_matches_builder_relayer_client_vector() {
    let private_key = "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80";
    let signer_wallet: PrivateKeySigner = private_key.parse().expect("known private key");
    let signer_addr = signer_wallet.address();

    let proxy_factory: Address = RELAYER_PROXY_FACTORY_POLYGON
        .parse()
        .expect("proxy factory");
    let relay_hub: Address = RELAYER_RELAY_HUB_POLYGON.parse().expect("relay hub");
    let relay_addr: Address = "0xae700edfd9ab986395f3999fe11177b9903a52f1"
        .parse()
        .expect("relay address");
    let usdc: Address = USDC_E_POLYGON.parse().expect("usdc");
    let approve_calldata = hex::decode(
        "095ea7b30000000000000000000000004d97dcd97ec945f40cf65f87097ace5ea0476045ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
    )
    .expect("approve calldata");
    let proxy_call_data =
        AutoClaimer::encode_proxy_transaction_data(usdc, approve_calldata).expect("proxy data");

    let struct_hash = AutoClaimer::create_proxy_struct_hash(
        signer_addr,
        proxy_factory,
        &proxy_call_data,
        U256::ZERO,
        U256::ZERO,
        U256::from(85_338u64),
        U256::ZERO,
        relay_hub,
        relay_addr,
    );
    let sig = ensure_0x_prefix(
        &signer_wallet
            .with_chain_id(Some(POLYGON_CHAIN_ID))
            .sign_message(struct_hash.as_slice())
            .await
            .expect("signature")
            .to_string(),
    );

    assert_eq!(
        sig,
        "0x4c18e2d2294a00d686714aff8e7936ab657cb4655dfccb2b556efadcb7e835f800dc2fecec69c501e29bb36ecb54b4da6b7c410c4dc740a33af2afde2b77297e1b"
    );
}
