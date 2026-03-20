use crate::main_runtime::enforce_coordinator_only_live;
use ploy::cli::runtime::SportsCommands;
use ploy::error::PloyError;
use ploy::error::Result;
use tracing::info;

pub(crate) async fn run_sports_command(cmd: &SportsCommands) -> Result<()> {
    match cmd {
        SportsCommands::SplitArb { dry_run, .. } => {
            info!("sports split-arb standalone runtime is retired");
            if !*dry_run {
                enforce_coordinator_only_live("ploy sports split-arb")?;
            }

            Err(PloyError::Validation(
                "standalone `ploy sports split-arb` runtime is retired; use canonical managed strategy deployments via `ploy platform start`, or use backtest/research tooling for offline analysis".to_string(),
            ))
        }
    }
}
