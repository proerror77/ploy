use super::proxy_support::{RelayerBuilderCredentials, ensure_0x_prefix, relayer_hmac_signature};
use super::*;

use ethers_core::types::{Address as EthersAddress, U256 as EthersU256};
use ethers_signers::{LocalWallet, Signer as _};
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
    let signer: EthersAddress = "0x9d699747148fd637a7d2514f9b3e3028bf59195c"
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
    let call_to: EthersAddress = "0x4D97DCd97eC945f40cF65F87097ACe5EA0476045"
        .parse()
        .expect("valid call target");
    let encoded = AutoClaimer::encode_proxy_transaction_data(call_to, vec![0x12, 0x34])
        .expect("proxy calldata should encode");
    assert!(!encoded.is_empty());
}

#[test]
fn test_encode_redeem_calldata_uses_standard_ctf_signature_for_binary_markets() {
    let encoded = AutoClaimer::encode_redeem_calldata(
        [0x11; 32],
        false,
        &[EthersU256::from(1_000_000u64), EthersU256::from(0u64)],
    )
    .expect("standard redeem calldata should encode");
    assert_eq!(hex::encode(&encoded[..4]), "9c542ed7");
}

#[test]
fn test_encode_redeem_calldata_uses_neg_risk_signature_for_neg_risk_markets() {
    let encoded = AutoClaimer::encode_redeem_calldata(
        [0x22; 32],
        true,
        &[EthersU256::from(500_000u64), EthersU256::from(500_000u64)],
    )
    .expect("neg-risk redeem calldata should encode");
    assert_eq!(hex::encode(&encoded[..4]), "d2d72a51");
}

#[test]
fn test_relayer_builder_credentials_debug_redacts_secrets() {
    let creds = RelayerBuilderCredentials {
        api_key: "api-key".to_string(),
        secret: "super-secret".to_string(),
        passphrase: "passphrase".to_string(),
    };
    let debug = format!("{creds:?}");
    assert!(!debug.contains("api-key"));
    assert!(!debug.contains("super-secret"));
    assert!(!debug.contains("passphrase"));
    assert!(debug.contains("[redacted]"));
}

#[tokio::test]
async fn test_proxy_signature_matches_builder_relayer_client_vector() {
    let private_key = "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80";
    let signer_wallet: LocalWallet = private_key.parse().expect("known private key");
    let signer_addr = signer_wallet.address();

    let proxy_factory: EthersAddress = RELAYER_PROXY_FACTORY_POLYGON
        .parse()
        .expect("proxy factory");
    let relay_hub: EthersAddress = RELAYER_RELAY_HUB_POLYGON.parse().expect("relay hub");
    let relay_addr: EthersAddress = "0xae700edfd9ab986395f3999fe11177b9903a52f1"
        .parse()
        .expect("relay address");
    let pusd: EthersAddress = PUSD_POLYGON.parse().expect("pusd");
    let approve_calldata = hex::decode(
        "095ea7b30000000000000000000000004d97dcd97ec945f40cf65f87097ace5ea0476045ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
    )
    .expect("approve calldata");
    let proxy_call_data =
        AutoClaimer::encode_proxy_transaction_data(pusd, approve_calldata).expect("proxy data");

    let struct_hash = AutoClaimer::create_proxy_struct_hash(
        signer_addr,
        proxy_factory,
        &proxy_call_data,
        EthersU256::zero(),
        EthersU256::zero(),
        EthersU256::from(85_338u64),
        EthersU256::zero(),
        relay_hub,
        relay_addr,
    );
    let sig = ensure_0x_prefix(
        &signer_wallet
            .with_chain_id(POLYGON_CHAIN_ID)
            .sign_message(struct_hash.as_bytes())
            .await
            .expect("signature")
            .to_string(),
    );

    assert_eq!(
        sig,
        "0x0357bad531e3207e34ca1b2f0ac6e3a54335a179c6dd3ab9c38e1f07fedf06ce1926d0037e2f364977b9a9964804a3681580937679a78e262dc4dd630348e22b1b"
    );
}
