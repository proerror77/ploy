use chrono::{DateTime, Utc};
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::collections::{HashMap, VecDeque};

use crate::error::{PloyError, Result};

pub const SEQ_LEN_5M: usize = 60;
pub const SEQ_LEN_15M: usize = 180;
pub const SEQ_FEATURE_DIM: usize = 11;

const GEQ_SETTLEMENT_BIAS: f64 = 0.002;

#[derive(Debug, Clone)]
pub struct SequenceSnapshot {
    pub ts: DateTime<Utc>,
    pub obi_5: Decimal,
    pub obi_10: Decimal,
    pub spread_bps: Decimal,
    pub bid_volume_5: Decimal,
    pub ask_volume_5: Decimal,
    pub momentum_1s: Decimal,
    pub momentum_5s: Decimal,
    pub spot_price: Decimal,
    pub remaining_secs: Decimal,
    pub price_to_beat: Decimal,
    pub distance_to_beat: Decimal,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SequenceAlignMode {
    Exact,
    TruncateOldest,
    LeftPadZero,
}

/// Standard normal CDF approximation (Abramowitz-Stegun), ~4dp accuracy.
fn normal_cdf(x: f64) -> f64 {
    let a1 = 0.254829592;
    let a2 = -0.284496736;
    let a3 = 1.421413741;
    let a4 = -1.453152027;
    let a5 = 1.061405429;
    let p = 0.3275911;

    let sign = if x < 0.0 { -1.0 } else { 1.0 };
    let z = x.abs() / std::f64::consts::SQRT_2;

    let t = 1.0 / (1.0 + p * z);
    let y = 1.0 - (((((a5 * t + a4) * t) + a3) * t + a2) * t + a1) * t * (-z * z).exp();

    0.5 * (1.0 + sign * y)
}

pub fn normalize_component(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push('_');
        }
    }
    if out.is_empty() {
        "unknown".to_string()
    } else {
        out
    }
}

pub fn normalize_timeframe(horizon: &str) -> String {
    let raw = horizon.trim().to_ascii_lowercase();
    if raw.contains("15m") || raw == "15" {
        "15m".to_string()
    } else if raw.contains("5m") || raw == "5" {
        "5m".to_string()
    } else if raw.is_empty() {
        "5m".to_string()
    } else {
        raw
    }
}

pub fn event_window_secs_for_horizon(horizon: &str) -> u64 {
    match normalize_timeframe(horizon).as_str() {
        "15m" => 15 * 60,
        "5m" => 5 * 60,
        _ => 5 * 60,
    }
}

pub fn sequence_len_for_horizon(horizon: &str) -> usize {
    match normalize_timeframe(horizon).as_str() {
        "15m" => SEQ_LEN_15M,
        _ => SEQ_LEN_5M,
    }
}

pub fn deployment_id_for(strategy: &str, coin: &str, horizon: &str) -> String {
    format!(
        "crypto.pm.{}.{}.{}",
        normalize_component(coin),
        normalize_timeframe(horizon),
        normalize_component(strategy)
    )
}

pub fn infer_coin_from_market_slug(slug: &str) -> String {
    slug.split('-')
        .next()
        .map(|s| s.trim().to_ascii_uppercase())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "UNKNOWN".to_string())
}

pub fn infer_horizon_from_market_slug(slug: &str) -> String {
    normalize_timeframe(slug)
}

pub fn push_sequence_snapshot(
    sequence_cache: &mut HashMap<String, VecDeque<SequenceSnapshot>>,
    key: &str,
    snapshot: SequenceSnapshot,
) {
    let window = sequence_cache.entry(key.to_string()).or_default();
    if let Some(last) = window.back_mut() {
        if last.ts == snapshot.ts {
            *last = snapshot;
            return;
        }
    }

    window.push_back(snapshot);
    while window.len() > SEQ_LEN_15M {
        let _ = window.pop_front();
    }
}

pub fn build_sequence(
    sequence_cache: &HashMap<String, VecDeque<SequenceSnapshot>>,
    key: &str,
    horizon: &str,
    feature_offsets: &[f32],
    feature_scales: &[f32],
) -> Option<Vec<f32>> {
    let seq_len = sequence_len_for_horizon(horizon);
    let window = sequence_cache.get(key)?;
    if window.len() < seq_len {
        return None;
    }

    let normalize =
        feature_offsets.len() == SEQ_FEATURE_DIM && feature_scales.len() == SEQ_FEATURE_DIM;

    let mut flat: Vec<f32> = Vec::with_capacity(seq_len * SEQ_FEATURE_DIM);
    let start_idx = window.len().saturating_sub(seq_len);
    for snap in window.iter().skip(start_idx) {
        let raw = [
            snap.obi_5.to_f64().unwrap_or(0.0) as f32,
            snap.obi_10.to_f64().unwrap_or(0.0) as f32,
            snap.spread_bps.to_f64().unwrap_or(0.0) as f32,
            snap.bid_volume_5.to_f64().unwrap_or(0.0) as f32,
            snap.ask_volume_5.to_f64().unwrap_or(0.0) as f32,
            snap.momentum_1s.to_f64().unwrap_or(0.0) as f32,
            snap.momentum_5s.to_f64().unwrap_or(0.0) as f32,
            snap.spot_price.to_f64().unwrap_or(0.0) as f32,
            snap.remaining_secs.to_f64().unwrap_or(0.0) as f32,
            snap.price_to_beat.to_f64().unwrap_or(0.0) as f32,
            snap.distance_to_beat.to_f64().unwrap_or(0.0) as f32,
        ];
        if normalize {
            for (i, v) in raw.iter().enumerate() {
                flat.push((v - feature_offsets[i]) * feature_scales[i]);
            }
        } else {
            flat.extend_from_slice(&raw);
        }
    }

    Some(flat)
}

pub fn estimate_p_up_gbm_anchor(
    spot_price: Decimal,
    start_price: Decimal,
    price_to_beat: Option<Decimal>,
    sigma_1s: Option<Decimal>,
    remaining_secs: i64,
    oracle_lag_buffer_secs: u64,
) -> Decimal {
    let remaining_secs = remaining_secs.max(0) as f64;
    let Some(sig_1s) = sigma_1s.and_then(|v| v.to_f64()) else {
        return dec!(0.50);
    };
    if !sig_1s.is_finite() || sig_1s <= 0.0 {
        return dec!(0.50);
    }

    let effective_remaining = if remaining_secs < 30.0 {
        remaining_secs + (oracle_lag_buffer_secs as f64)
    } else {
        remaining_secs
    };
    let sigma_rem = sig_1s * effective_remaining.sqrt();
    if !sigma_rem.is_finite() || sigma_rem <= 0.0 {
        return dec!(0.50);
    }

    let spot = spot_price.to_f64().unwrap_or(0.0);
    if !spot.is_finite() || spot <= 0.0 {
        return dec!(0.50);
    }

    if let Some(beat) = price_to_beat {
        let beat_f = beat.to_f64().unwrap_or(0.0);
        if beat_f.is_finite() && beat_f > 0.0 {
            let required_return = (beat_f - spot) / spot;
            if required_return.is_finite() {
                let p = (1.0 - normal_cdf(required_return / sigma_rem)).clamp(0.001, 0.999);
                return Decimal::from_f64_retain(p).unwrap_or(dec!(0.50));
            }
        }
    }

    let start_f = start_price.to_f64().unwrap_or(0.0);
    if !start_f.is_finite() || start_f <= 0.0 {
        return dec!(0.50);
    }
    let window_move = (spot - start_f) / start_f;
    if !window_move.is_finite() {
        return dec!(0.50);
    }

    let mut p = normal_cdf(window_move / sigma_rem).clamp(0.001, 0.999);
    p += GEQ_SETTLEMENT_BIAS;

    Decimal::from_f64_retain(p.clamp(0.001, 0.999)).unwrap_or(dec!(0.50))
}

pub fn align_sequence_to_model_input(
    sequence: &[f32],
    model_input_dim: usize,
) -> Result<(Vec<f32>, SequenceAlignMode)> {
    if model_input_dim == 0 {
        return Err(PloyError::Validation(
            "onnx model input_dim must be > 0".to_string(),
        ));
    }
    if model_input_dim % SEQ_FEATURE_DIM != 0 {
        return Err(PloyError::Validation(format!(
            "onnx model input_dim {} must be a multiple of sequence feature dim {}",
            model_input_dim, SEQ_FEATURE_DIM
        )));
    }
    if sequence.len() % SEQ_FEATURE_DIM != 0 {
        return Err(PloyError::Validation(format!(
            "sequence input dim {} must be a multiple of sequence feature dim {}",
            sequence.len(),
            SEQ_FEATURE_DIM
        )));
    }

    let model_snapshots = model_input_dim / SEQ_FEATURE_DIM;
    let sequence_snapshots = sequence.len() / SEQ_FEATURE_DIM;

    if sequence_snapshots == model_snapshots {
        return Ok((sequence.to_vec(), SequenceAlignMode::Exact));
    }

    if sequence_snapshots > model_snapshots {
        let start_snapshot = sequence_snapshots - model_snapshots;
        let start = start_snapshot * SEQ_FEATURE_DIM;
        return Ok((
            sequence[start..].to_vec(),
            SequenceAlignMode::TruncateOldest,
        ));
    }

    let pad_snapshots = model_snapshots - sequence_snapshots;
    let mut aligned = vec![0.0f32; pad_snapshots * SEQ_FEATURE_DIM];
    aligned.extend_from_slice(sequence);
    Ok((aligned, SequenceAlignMode::LeftPadZero))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_sequence_snapshot(ts: DateTime<Utc>) -> SequenceSnapshot {
        SequenceSnapshot {
            ts,
            obi_5: dec!(0.10),
            obi_10: dec!(0.12),
            spread_bps: dec!(2.0),
            bid_volume_5: dec!(1000),
            ask_volume_5: dec!(980),
            momentum_1s: dec!(0.001),
            momentum_5s: dec!(0.003),
            spot_price: dec!(102000),
            remaining_secs: dec!(90),
            price_to_beat: dec!(102500),
            distance_to_beat: dec!(0.0049),
        }
    }

    #[test]
    fn build_sequence_lengths_for_5m_and_15m() {
        let mut cache: HashMap<String, VecDeque<SequenceSnapshot>> = HashMap::new();
        let key = "BTCUSDT|15m";
        let now = Utc::now();
        for i in 0..SEQ_LEN_15M {
            push_sequence_snapshot(
                &mut cache,
                key,
                sample_sequence_snapshot(now + chrono::Duration::seconds(i as i64)),
            );
        }

        let seq_5m =
            build_sequence(&cache, key, "5m", &[], &[]).expect("5m sequence should be available");
        assert_eq!(seq_5m.len(), SEQ_LEN_5M * SEQ_FEATURE_DIM);

        let seq_15m =
            build_sequence(&cache, key, "15m", &[], &[]).expect("15m sequence should be available");
        assert_eq!(seq_15m.len(), SEQ_LEN_15M * SEQ_FEATURE_DIM);
    }

    #[test]
    fn build_sequence_returns_none_when_insufficient_history() {
        let mut cache: HashMap<String, VecDeque<SequenceSnapshot>> = HashMap::new();
        let key = "BTCUSDT|5m";
        let now = Utc::now();
        for i in 0..(SEQ_LEN_5M - 1) {
            push_sequence_snapshot(
                &mut cache,
                key,
                sample_sequence_snapshot(now + chrono::Duration::seconds(i as i64)),
            );
        }

        assert!(build_sequence(&cache, key, "5m", &[], &[]).is_none());
    }

    #[test]
    fn build_sequence_applies_normalization() {
        let mut cache: HashMap<String, VecDeque<SequenceSnapshot>> = HashMap::new();
        let key = "BTCUSDT|5m";
        let now = Utc::now();
        for i in 0..SEQ_LEN_5M {
            push_sequence_snapshot(
                &mut cache,
                key,
                sample_sequence_snapshot(now + chrono::Duration::seconds(i as i64)),
            );
        }

        let raw = build_sequence(&cache, key, "5m", &[], &[]).expect("raw sequence");

        let offsets = vec![0.0f32; SEQ_FEATURE_DIM];
        let scales = vec![1.0f32; SEQ_FEATURE_DIM];
        let identity =
            build_sequence(&cache, key, "5m", &offsets, &scales).expect("identity normalized");
        assert_eq!(raw.len(), identity.len());
        for (a, b) in raw.iter().zip(identity.iter()) {
            assert!((a - b).abs() < 1e-6, "identity transform should match raw");
        }

        let mut offsets2 = vec![0.0f32; SEQ_FEATURE_DIM];
        let mut scales2 = vec![1.0f32; SEQ_FEATURE_DIM];
        offsets2[0] = 1.0;
        scales2[0] = 2.0;
        let normed = build_sequence(&cache, key, "5m", &offsets2, &scales2).expect("normalized");
        let expected_first = (raw[0] - 1.0) * 2.0;
        assert!(
            (normed[0] - expected_first).abs() < 1e-6,
            "expected {expected_first}, got {}",
            normed[0]
        );
        assert!(
            (normed[1] - raw[1]).abs() < 1e-6,
            "second feature should be unchanged"
        );
    }

    #[test]
    fn align_sequence_to_model_input_handles_boundary_cases() {
        let exact = vec![1.0f32; SEQ_FEATURE_DIM * 2];
        let (exact_aligned, exact_mode) =
            align_sequence_to_model_input(&exact, SEQ_FEATURE_DIM * 2).unwrap();
        assert_eq!(exact_mode, SequenceAlignMode::Exact);
        assert_eq!(exact_aligned, exact);
    }

    #[test]
    fn align_sequence_to_model_input_rejects_non_snapshot_aligned_model_dim() {
        let sequence = vec![1.0f32; SEQ_FEATURE_DIM * 2];
        let err = align_sequence_to_model_input(&sequence, SEQ_FEATURE_DIM + 1)
            .err()
            .expect("non-snapshot input_dim must fail fast");
        assert!(err
            .to_string()
            .contains("must be a multiple of sequence feature dim"));
    }

    #[test]
    fn align_sequence_to_model_input_truncate_and_pad_keep_snapshot_boundaries() {
        let mut sequence = Vec::new();
        for snapshot_value in [1.0f32, 2.0, 3.0] {
            sequence.extend(std::iter::repeat(snapshot_value).take(SEQ_FEATURE_DIM));
        }

        let (truncated, truncate_mode) =
            align_sequence_to_model_input(&sequence, SEQ_FEATURE_DIM * 2).unwrap();
        assert_eq!(truncate_mode, SequenceAlignMode::TruncateOldest);
        assert_eq!(truncated.len(), SEQ_FEATURE_DIM * 2);
        assert!(truncated[..SEQ_FEATURE_DIM].iter().all(|v| *v == 2.0));
        assert!(truncated[SEQ_FEATURE_DIM..].iter().all(|v| *v == 3.0));

        let (padded, pad_mode) =
            align_sequence_to_model_input(&sequence, SEQ_FEATURE_DIM * 4).unwrap();
        assert_eq!(pad_mode, SequenceAlignMode::LeftPadZero);
        assert_eq!(padded.len(), SEQ_FEATURE_DIM * 4);
        assert!(padded[..SEQ_FEATURE_DIM].iter().all(|v| *v == 0.0));
        assert_eq!(&padded[SEQ_FEATURE_DIM..], sequence.as_slice());
    }

    #[test]
    fn align_sequence_to_model_input_allows_15m_sequence_with_5m_model_dim() {
        let seq_15m_len = SEQ_LEN_15M * SEQ_FEATURE_DIM;
        let seq_5m_len = SEQ_LEN_5M * SEQ_FEATURE_DIM;
        let seq_15m: Vec<f32> = (0..seq_15m_len).map(|i| i as f32).collect();

        let (aligned, mode) = align_sequence_to_model_input(&seq_15m, seq_5m_len).unwrap();

        assert_eq!(mode, SequenceAlignMode::TruncateOldest);
        assert_eq!(aligned.len(), seq_5m_len);
        assert_eq!(aligned, seq_15m[(seq_15m_len - seq_5m_len)..].to_vec());
    }

    #[test]
    fn deployment_metadata_helpers() {
        assert_eq!(normalize_timeframe("15"), "15m");
        assert_eq!(normalize_timeframe("btc-5m"), "5m");
        assert_eq!(event_window_secs_for_horizon("15m"), 900);
        assert_eq!(event_window_secs_for_horizon("5m"), 300);
        assert_eq!(
            deployment_id_for("crypto_lob_ml", "ETH", "5m"),
            "crypto.pm.eth.5m.crypto_lob_ml"
        );
    }
}
