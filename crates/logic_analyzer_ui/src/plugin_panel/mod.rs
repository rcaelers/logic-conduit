//! Compile-time plugin panel contracts and application registry.

mod contract;
mod registration;
mod registry;

pub use contract::{PluginPanel, PluginPanelContext, PluginPanelIcon};
pub use registration::UiPanelRegistration;
pub(crate) use registry::{PluginPanelRegistry, PluginPanels, PluginPanelsState};
