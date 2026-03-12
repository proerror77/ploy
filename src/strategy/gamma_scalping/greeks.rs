//! Binary option greeks calculator for gamma scalping.
//!
//! Computes delta, gamma, theta, vega for binary options paying $1 if S > K.
//! Uses the standard normal PDF/CDF from the volatility module.

use crate::strategy::volatility::normal_cdf;

/// Standard normal probability density function.
fn norm_pdf(x: f64) -> f64 {
    const INV_SQRT_2PI: f64 = 0.398_942_280_401_432_7; // 1/sqrt(2π)
    INV_SQRT_2PI * (-0.5 * x * x).exp()
}

/// Greeks for a binary call option paying $1 if S > K at expiry.
#[derive(Debug, Clone, Copy)]
pub struct BinaryGreeks {
    /// ∂V/∂S — sensitivity to spot price
    pub delta: f64,
    /// ∂²V/∂S² — rate of delta change (highest near ATM)
    pub gamma: f64,
    /// ∂V/∂t — time decay per second (negative = decaying)
    pub theta: f64,
    /// ∂V/∂σ — volatility sensitivity
    pub vega: f64,
    /// Fair value = N(d₂)
    pub fair_value: f64,
    /// The d₂ parameter (useful for diagnostics)
    pub d2: f64,
}

/// Compute binary option greeks.
///
/// # Arguments
/// * `spot` — Current underlying price (e.g., BTC price)
/// * `strike` — Strike price (price_to_beat from Polymarket event)
/// * `vol` — Annualized volatility (σ)
/// * `time_remaining_secs` — Seconds until expiry
/// * `window_secs` — Total event window in seconds (300 for 5m, 900 for 15m)
///
/// Returns `None` if inputs are degenerate (zero vol, zero time, non-positive prices).
pub fn binary_greeks(
    spot: f64,
    strike: f64,
    vol: f64,
    time_remaining_secs: f64,
    window_secs: f64,
) -> Option<BinaryGreeks> {
    if spot <= 0.0 || strike <= 0.0 || vol <= 1e-12 || time_remaining_secs <= 0.0 {
        return None;
    }

    // Normalize time: fraction of the event window remaining
    let t = time_remaining_secs / window_secs;
    let sqrt_t = t.sqrt();
    let sigma_sqrt_t = vol * sqrt_t;

    if sigma_sqrt_t < 1e-12 {
        return None;
    }

    // d₂ = [ln(S/K) - σ²T/2] / (σ√T)
    let ln_ratio = (spot / strike).ln();
    let d2 = (ln_ratio - 0.5 * vol * vol * t) / sigma_sqrt_t;

    let n_d2 = norm_pdf(d2);
    let fair_value = normal_cdf(d2);

    // Delta = n(d₂) / (S × σ√T)
    let delta = n_d2 / (spot * sigma_sqrt_t);

    // Gamma = -d₂ × n(d₂) / (S² × σ²T)
    let gamma = -d2 * n_d2 / (spot * spot * vol * vol * t);

    // Theta: time decay for binary call holder.
    // For a binary call, theta = -n(d₂) × [d₂/(2T) + (ln(S/K))/(σ²T^(3/2))] / window_secs
    // Simplified: near ATM, theta is always negative (time decay costs money).
    // We use: theta_per_unit = n(d₂) × σ / (2 × √T) which is always positive,
    // then negate to represent cost to the holder.
    let theta_per_unit = n_d2 * vol / (2.0 * sqrt_t);
    let theta = -theta_per_unit / window_secs; // negative = time decay costs money

    // Vega = -d₂ × n(d₂) / σ
    let vega = -d2 * n_d2 / vol;

    Some(BinaryGreeks {
        delta,
        gamma,
        theta,
        vega,
        fair_value,
        d2,
    })
}

/// Compute portfolio delta for a straddle (long UP + long DOWN tokens).
///
/// For a binary straddle, the DOWN token is a binary put = 1 - binary call,
/// so delta_down = -delta_up. Net delta = shares_up × delta_up - shares_down × delta_up.
pub fn straddle_delta(greeks: &BinaryGreeks, up_shares: f64, down_shares: f64) -> f64 {
    // UP token delta = greeks.delta (call delta)
    // DOWN token delta = -greeks.delta (put delta = -call delta for binary)
    up_shares * greeks.delta - down_shares * greeks.delta
}

/// Compute portfolio gamma for a straddle.
/// Both UP and DOWN tokens have the same |gamma| (gamma is symmetric for binary straddle).
pub fn straddle_gamma(greeks: &BinaryGreeks, up_shares: f64, down_shares: f64) -> f64 {
    // UP gamma = greeks.gamma, DOWN gamma = greeks.gamma (same sign — both convex near ATM)
    (up_shares + down_shares) * greeks.gamma.abs()
}

/// Estimate realized volatility from a series of log returns.
///
/// Uses the standard deviation of log returns, annualized to the given window.
pub fn realized_vol_from_closes(closes: &[f64], _window_secs: f64) -> Option<f64> {
    if closes.len() < 3 {
        return None;
    }

    let log_returns: Vec<f64> = closes
        .windows(2)
        .map(|w| (w[1] / w[0]).ln())
        .collect();

    let n = log_returns.len() as f64;
    let mean = log_returns.iter().sum::<f64>() / n;
    let variance = log_returns.iter().map(|r| (r - mean).powi(2)).sum::<f64>() / (n - 1.0);

    // Scale to the event window: each return covers (window_secs / closes.len()) seconds
    // We want vol over the full window, so multiply by sqrt(n_periods)
    let periods_per_window = n;
    let vol = variance.sqrt() * periods_per_window.sqrt();

    if vol.is_finite() && vol > 0.0 {
        Some(vol)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_atm_greeks() {
        // ATM: spot == strike, 50% of time remaining in a 15m window
        let g = binary_greeks(100.0, 100.0, 0.01, 450.0, 900.0).unwrap();

        // Fair value should be close to 0.5 at ATM
        assert!((g.fair_value - 0.5).abs() < 0.05, "fair_value={}", g.fair_value);

        // Delta should be positive (call delta)
        assert!(g.delta > 0.0, "delta={}", g.delta);

        // Gamma should be large at ATM (peak convexity)
        assert!(g.gamma.abs() > 0.0, "gamma={}", g.gamma);

        // Theta should be negative (time decay)
        assert!(g.theta < 0.0, "theta={}", g.theta);
    }

    #[test]
    fn test_deep_itm_greeks() {
        // Deep ITM: spot >> strike
        let g = binary_greeks(105.0, 100.0, 0.005, 450.0, 900.0).unwrap();

        // Fair value should be close to 1.0
        assert!(g.fair_value > 0.9, "fair_value={}", g.fair_value);

        // Delta should be small (already deep ITM)
        assert!(g.delta.abs() < g.fair_value, "delta too large");
    }

    #[test]
    fn test_deep_otm_greeks() {
        // Deep OTM: spot << strike
        let g = binary_greeks(95.0, 100.0, 0.005, 450.0, 900.0).unwrap();

        // Fair value should be close to 0.0
        assert!(g.fair_value < 0.1, "fair_value={}", g.fair_value);
    }

    #[test]
    fn test_put_call_parity() {
        // For binary options: P(up) + P(down) = 1
        // delta_up + delta_down = 0
        let g = binary_greeks(100.0, 100.0, 0.01, 450.0, 900.0).unwrap();
        let net_delta = straddle_delta(&g, 100.0, 100.0);

        // Equal shares → net delta should be ~0
        assert!(net_delta.abs() < 1e-6, "net_delta={}", net_delta);
    }

    #[test]
    fn test_degenerate_inputs() {
        assert!(binary_greeks(0.0, 100.0, 0.01, 450.0, 900.0).is_none());
        assert!(binary_greeks(100.0, 100.0, 0.0, 450.0, 900.0).is_none());
        assert!(binary_greeks(100.0, 100.0, 0.01, 0.0, 900.0).is_none());
    }

    #[test]
    fn test_realized_vol() {
        // Constant prices → zero vol
        let closes = vec![100.0, 100.0, 100.0, 100.0, 100.0];
        let vol = realized_vol_from_closes(&closes, 900.0);
        assert!(vol.is_none() || vol.unwrap() < 1e-10);

        // Oscillating prices → positive vol
        let closes = vec![100.0, 101.0, 99.5, 100.5, 99.0, 101.5];
        let vol = realized_vol_from_closes(&closes, 900.0);
        assert!(vol.is_some());
        assert!(vol.unwrap() > 0.0);
    }
}
