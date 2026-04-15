pub mod registry;
pub mod scan;
pub use registry::{FactorMeta, FactorRegistry, Regime};
pub use scan::scan_into_registry;
