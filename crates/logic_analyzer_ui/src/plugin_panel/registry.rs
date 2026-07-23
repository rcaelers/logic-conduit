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

    pub(crate) fn show(
        &mut self,
        content_id: &str,
        panel_id: &str,
        ui: &mut egui::Ui,
    ) -> Option<String> {
        let registered = self
            .registry
            .panels
            .iter()
            .find(|panel| panel.definition.stable_id == content_id)?;
        let key = (content_id.to_owned(), panel_id.to_owned());
        let mut restore_warning = None;
        let panel = self.instances.entry(key).or_insert_with(|| {
            let mut panel = (registered.factory)();
            if let Some(state) = self
                .restored
                .panels
                .get(content_id)
                .and_then(|panels| panels.get(panel_id))
                .cloned()
                && let Err(error) = panel.restore_state(state)
            {
                restore_warning = Some(format!(
                    "Could not restore saved state for plugin panel '{}': {error}",
                    registered.definition.title
                ));
            }
            panel
        });
        let lanes: Vec<OpaqueCollectedLane> = self.lanes.opaque_lanes();
        panel.show(ui, PluginPanelContext::new(&lanes));
        restore_warning
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

    #[derive(Default)]
    struct RejectingStatePanel;

    impl PluginPanel for RejectingStatePanel {
        fn show(&mut self, _ui: &mut egui::Ui, _context: PluginPanelContext<'_>) {}

        fn restore_state(&mut self, _state: serde_json::Value) -> Result<(), String> {
            Err("unsupported state version 9".to_owned())
        }
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

    #[test]
    fn invalid_saved_panel_state_produces_one_user_facing_diagnostic() {
        let mut registry = PluginPanelRegistry::default();
        registry
            .register::<RejectingStatePanel>(PluginPanelDescriptor::new(
                "org.example.camera/v1",
                "Camera",
            ))
            .unwrap();
        let mut panels = PluginPanels::new(registry);
        panels.restore_state(PluginPanelsState {
            panels: HashMap::from([(
                "org.example.camera/v1".to_owned(),
                HashMap::from([("panel-1".to_owned(), serde_json::json!({ "version": 9 }))]),
            )]),
        });

        let context = egui::Context::default();
        context.begin_pass(egui::RawInput::default());
        let mut ui = egui::Ui::new(
            context.clone(),
            egui::Id::new("plugin-panel-state-test"),
            egui::UiBuilder::new(),
        );
        let first_warning = panels.show("org.example.camera/v1", "panel-1", &mut ui);
        let second_warning = panels.show("org.example.camera/v1", "panel-1", &mut ui);
        let _ = context.end_pass();

        assert!(
            first_warning
                .as_deref()
                .is_some_and(|warning| warning.contains("unsupported state version 9"))
        );
        assert_eq!(second_warning, None);
    }
}
