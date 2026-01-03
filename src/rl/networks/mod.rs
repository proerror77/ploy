//! Neural Network Architectures
//!
//! Actor and Critic networks for policy gradient methods.

pub mod encoder;
pub mod actor;
pub mod critic;

pub use encoder::StateEncoderNetwork;
pub use actor::Actor;
pub use critic::Critic;
