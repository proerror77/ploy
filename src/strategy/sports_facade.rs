pub use super::nba_comeback::nba_data_collector::{
    CollectorConfig as NbaCollectorConfig, DataCollector as NbaDataCollector,
    GameState as NbaGameState, MarketSnapshot as NbaMarketSnapshot, OrderbookData, TeamStats,
};
pub use super::nba_comeback::nba_entry::{
    EntryConfig, EntryDecision, EntryLogic, EntrySignal, PartialSignal,
};
pub use super::nba_comeback::nba_exit::{
    ExitConfig as NbaExitConfig, ExitDecision, ExitLogic, ExitUrgency, PositionState,
};
pub use super::nba_comeback::nba_filters::{
    FilterConfig, FilterResult, MarketContext, MarketFilters,
};
pub use super::nba_comeback::nba_state_machine::{
    StateEvent as NbaStateEvent, StateMachine as NbaStateMachine, StrategyState as NbaStrategyState,
};
pub use super::nba_comeback::nba_winprob::{
    GameFeatures, LiveWinProbModel, ModelMetadata, WinProbCoefficients, WinProbPrediction,
};
pub use super::sports::{SportsLeague, SportsMarketDiscovery};

#[cfg(test)]
mod tests {
    use crate::strategy::{
        EntryConfig, GameFeatures, NbaCollectorConfig, NbaExitConfig, NbaStateEvent, SportsLeague,
        SportsMarketDiscovery,
    };

    #[test]
    fn root_strategy_module_reexports_sports_surface() {
        let _: Option<SportsLeague> = None;
        let _: Option<SportsMarketDiscovery> = None;
        let _: Option<NbaCollectorConfig> = None;
        let _: Option<EntryConfig> = None;
        let _: Option<NbaExitConfig> = None;
        let _: Option<NbaStateEvent> = None;
        let _: Option<GameFeatures> = None;
    }
}
