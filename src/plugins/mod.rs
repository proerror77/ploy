pub mod definition;
pub mod deployment;
pub mod projector;
pub mod registry;
pub mod spec;

pub use definition::{PluginDefinition, PluginKind};
pub use deployment::{DeploymentState, PluginDeployment};
pub use projector::ProjectedRuntimeSpec;
pub use registry::PluginRegistry;
pub use spec::{ComposableCryptoSpec, PluginSpec, RegisteredStrategySpec};
