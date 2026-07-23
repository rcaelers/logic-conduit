//! Compile-time plugin panel contracts and application registry.

mod contract;
mod registry;

pub use contract::{PluginPanel, PluginPanelContext, PluginPanelDescriptor, PluginPanelIcon};
pub(crate) use registry::{PluginPanelRegistry, PluginPanels, PluginPanelsState};
