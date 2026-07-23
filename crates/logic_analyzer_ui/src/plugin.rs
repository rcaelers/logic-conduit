//! Application-level compile-time plugin registration.

use crate::plugin_panel::{PluginPanel, PluginPanelDescriptor, PluginPanelRegistry};

/// Composes graph/runtime registrations with UI panel registrations.
pub struct PluginContext<'a> {
    graph: logic_analyzer_graph::PluginContext<'a>,
    panels: &'a mut PluginPanelRegistry,
}

impl<'a> PluginContext<'a> {
    pub(crate) fn new(
        graph: logic_analyzer_graph::PluginContext<'a>,
        panels: &'a mut PluginPanelRegistry,
    ) -> Self {
        Self { graph, panels }
    }

    pub fn graph(&mut self) -> &mut logic_analyzer_graph::PluginContext<'a> {
        &mut self.graph
    }

    pub fn register_panel<P>(
        &mut self,
        descriptor: PluginPanelDescriptor,
    ) -> Result<&mut Self, String>
    where
        P: PluginPanel + Default + 'static,
    {
        self.panels.register::<P>(descriptor)?;
        Ok(self)
    }
}
