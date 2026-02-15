//! Neural Network Architectures
//!
//! Actor and Critic networks for policy gradient methods.

pub mod actor;
pub mod critic;
pub mod encoder;

pub use actor::Actor;
pub use critic::Critic;
pub use encoder::StateEncoderNetwork;
