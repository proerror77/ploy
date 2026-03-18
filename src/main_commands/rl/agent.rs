#[cfg(feature = "rl")]
use ploy::error::Result;

#[cfg(feature = "rl")]
#[allow(clippy::too_many_arguments)]
pub(super) async fn run_agent(
    _symbol: &str,
    _market: &str,
    _up_token: &str,
    _down_token: &str,
    _shares: u64,
    _max_exposure: f64,
    _exploration: f32,
    _online_learning: bool,
    _dry_run: bool,
    _tick_interval: u64,
    _policy_onnx: &Option<String>,
    _policy_output: &str,
    _policy_version: &Option<String>,
) -> Result<()> {
    anyhow::bail!(
        "RLCryptoAgent (push-based) has been removed. Use the pull-based agent system instead."
    )
}
