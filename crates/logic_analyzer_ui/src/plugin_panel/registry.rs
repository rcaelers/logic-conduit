//! Runtime instances for registered plugin panel kinds.

use std::collections::HashMap;
use std::sync::Arc;

use signal_processing::{DerivedLanes, OpaqueCollectedLane};

use super::contract::{PluginPanel, PluginPanelContext, PluginPanelDescriptor, PluginPanelIcon};

type PluginPanelFactory = Arc<dyn Fn() -> Box<dyn PluginPanel> + Send + Sync>;

#[derive(Clone)]
pub(crate) struct PluginPanelDefinition {
    pub(crate) stable_id: String,
    pub(crate) title: String,
    pub(crate) icon: PluginPanelIcon,
    pub(crate) minimum_width: f32,
    pub(crate) minimum_height: f32,
    pub(crate) singleton: bool,
}

struct RegisteredPluginPanel {
    definition: PluginPanelDefinition,
    factory: PluginPanelFactory,
}

#[derive(Default)]
pub(crate) struct PluginPanelRegistry {
    panels: Vec<RegisteredPluginPanel>,
}

impl PluginPanelRegistry {
    pub(crate) fn register<P>(&mut self, descriptor: PluginPanelDescriptor) -> Result<(), String>
    where
        P: PluginPanel + Default + 'static,
    {
        if descriptor.stable_id.trim().is_empty() {
            return Err("plugin panel identifiers must not be empty".to_owned());
        }
        if self
            .panels
            .iter()
            .any(|panel| panel.definition.stable_id == descriptor.stable_id)
        {
            return Err(format!(
                "plugin panel '{}' is already registered",
                descriptor.stable_id
            ));
        }
        self.panels.push(RegisteredPluginPanel {
            definition: PluginPanelDefinition {
                stable_id: descriptor.stable_id,
                title: descriptor.title,
                icon: descriptor.icon,
                minimum_width: descriptor.minimum_width,
                minimum_height: descriptor.minimum_height,
                singleton: descriptor.singleton,
            },
            factory: Arc::new(|| Box::<P>::default()),
        });
        Ok(())
    }
}

#[derive(Clone, Default, serde::Deserialize, serde::Serialize)]
pub(crate) struct PluginPanelsState {
    panels: HashMap<String, HashMap<String, serde_json::Value>>,
}

pub(crate) struct PluginPanels {
    registry: PluginPanelRegistry,
    instances: HashMap<(String, String), Box<dyn PluginPanel>>,
    restored: PluginPanelsState,
    lanes: DerivedLanes,
}

impl PluginPanels {
    pub(crate) fn new(registry: PluginPanelRegistry) -> Self {
        Self {
            registry,
            instances: HashMap::new(),
            restored: PluginPanelsState::default(),
            lanes: DerivedLanes::new(),
        }
    }

    pub(crate) fn definitions(&self) -> Vec<PluginPanelDefinition> {
        self.registry
            .panels
            .iter()
            .map(|panel| panel.definition.clone())
            .collect()
    }

    pub(crate) fn set_run_data(&mut self, lanes: DerivedLanes) {
        self.lanes = lanes;
    }

    pub(crate) fn restore_state(&mut self, state: PluginPanelsState) {
        self.instances.clear();
        self.restored = state;
    }

    pub(crate) fn reset_state(&mut self) {
        self.restore_state(PluginPanelsState::default());
    }

    pub(crate) fn state(&self) -> PluginPanelsState {
        let mut state = self.restored.clone();
        for ((content_id, panel_id), panel) in &self.instances {
            state
                .panels
                .entry(content_id.clone())
                .or_default()
                .insert(panel_id.clone(), panel.save_state());
        }
        state
    }

    pub(crate) fn show(&mut self, content_id: &str, panel_id: &str, ui: &mut egui::Ui) -> bool {
        let Some(registered) = self
            .registry
            .panels
            .iter()
            .find(|panel| panel.definition.stable_id == content_id)
        else {
            return false;
        };
        let key = (content_id.to_owned(), panel_id.to_owned());
        let panel = self.instances.entry(key).or_insert_with(|| {
            let mut panel = (registered.factory)();
            if let Some(state) = self
                .restored
                .panels
                .get(content_id)
                .and_then(|panels| panels.get(panel_id))
                .cloned()
            {
                let _ = panel.restore_state(state);
            }
            panel
        });
        let lanes: Vec<OpaqueCollectedLane> = self.lanes.opaque_lanes();
        panel.show(ui, PluginPanelContext::new(&lanes));
        true
    }
}

#[cfg(test)]
mod registry_tests {
    use super::*;

    #[derive(Default)]
    struct TestPanel;

    impl PluginPanel for TestPanel {
        fn show(&mut self, _ui: &mut egui::Ui, _context: PluginPanelContext<'_>) {}
    }

    #[test]
    fn registered_panel_is_discoverable_without_application_dispatch_changes() {
        let mut registry = PluginPanelRegistry::default();
        registry
            .register::<TestPanel>(
                PluginPanelDescriptor::new("org.example.camera/v1", "Camera")
                    .icon(PluginPanelIcon::Image),
            )
            .unwrap();
        let panels = PluginPanels::new(registry);
        let definitions = panels.definitions();

        assert_eq!(definitions.len(), 1);
        assert_eq!(definitions[0].stable_id, "org.example.camera/v1");
        assert_eq!(definitions[0].icon, PluginPanelIcon::Image);
    }

    #[test]
    fn duplicate_stable_panel_identity_is_rejected() {
        let mut registry = PluginPanelRegistry::default();
        let descriptor = PluginPanelDescriptor::new("org.example.camera/v1", "Camera");
        registry.register::<TestPanel>(descriptor.clone()).unwrap();

        assert!(registry.register::<TestPanel>(descriptor).is_err());
    }
}
