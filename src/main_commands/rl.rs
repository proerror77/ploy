#[cfg(feature = "rl")]
use ploy::cli::runtime::RlCommands;
#[cfg(feature = "rl")]
use ploy::error::Result;

#[cfg(feature = "rl")]
mod agent;
#[cfg(feature = "rl")]
mod backtest;
#[cfg(feature = "rl")]
mod core_modes;
#[cfg(feature = "rl")]
mod lead_lag;

/// RL strategy commands
#[cfg(feature = "rl")]
pub(crate) async fn run_rl_command(cmd: &RlCommands) -> Result<()> {
    match cmd {
        RlCommands::Train {
            episodes,
            checkpoint,
            lr,
            batch_size,
            update_freq,
            series,
            symbol,
            resume,
            verbose,
        } => {
            core_modes::run_train(
                *episodes,
                checkpoint,
                *lr,
                *batch_size,
                *update_freq,
                series,
                symbol,
                resume,
                *verbose,
            )
            .await?;
        }

        RlCommands::Run {
            model,
            online_learning,
            series,
            symbol,
            exploration,
            dry_run,
        } => {
            core_modes::run_strategy(
                model,
                *online_learning,
                series,
                symbol,
                *exploration,
                *dry_run,
            )
            .await?;
        }

        RlCommands::Eval {
            model,
            data,
            episodes,
            output,
        } => {
            core_modes::run_eval(model, data, *episodes, output).await?;
        }

        RlCommands::Info { model } => {
            core_modes::run_info(model).await?;
        }

        RlCommands::Export {
            model,
            format,
            output,
        } => {
            core_modes::run_export(model, format, output).await?;
        }

        RlCommands::Backtest {
            episodes,
            duration,
            volatility,
            round,
            capital,
            verbose,
        } => {
            backtest::run_backtest(*episodes, *duration, *volatility, round, *capital, *verbose)
                .await?;
        }

        RlCommands::LeadLag {
            episodes,
            trade_size,
            max_position,
            symbol,
            lr: _lr,
            checkpoint,
            verbose,
        } => {
            lead_lag::run_lead_lag(
                *episodes,
                *trade_size,
                *max_position,
                symbol,
                checkpoint,
                *verbose,
            )
            .await?;
        }

        RlCommands::LeadLagLive {
            symbol,
            trade_size,
            max_position,
            market,
            checkpoint,
            dry_run,
            min_confidence,
        } => {
            lead_lag::run_lead_lag_live(
                symbol,
                *trade_size,
                *max_position,
                market,
                checkpoint,
                *dry_run,
                *min_confidence,
            )
            .await?;
        }

        RlCommands::Agent {
            symbol,
            market,
            up_token,
            down_token,
            shares,
            max_exposure,
            exploration,
            online_learning,
            dry_run,
            tick_interval,
            policy_onnx,
            policy_output,
            policy_version,
        } => {
            agent::run_agent(
                symbol,
                market,
                up_token,
                down_token,
                *shares,
                *max_exposure,
                *exploration,
                *online_learning,
                *dry_run,
                *tick_interval,
                policy_onnx,
                policy_output,
                policy_version,
            )
            .await?;
        }
    }

    Ok(())
}
