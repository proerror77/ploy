//! Strategy implementations.

pub mod directional;
pub mod directional_bayes;

pub use directional::DirectionalStrategy;
pub use directional_bayes::BayesianDirectionalStrategy;
