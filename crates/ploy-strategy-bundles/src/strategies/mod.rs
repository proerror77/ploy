//! Strategy implementations.

pub mod directional;
pub mod directional_bayes;
pub mod mean_reversion;

pub use directional::DirectionalStrategy;
pub use directional_bayes::BayesianDirectionalStrategy;
pub use mean_reversion::MeanReversionStrategy;
