//! Inventory contract for one independently openable application panel.

use std::collections::HashSet;

use super::contract::{PluginPanel, PluginPanelDescriptor, PluginPanelIcon};
use super::registry::PluginPanelRegistry;

/// Compile-time registration for one persistable panel kind.
pub struct UiPanelRegistration {
    stable_id: &'static str,
    title: &'static str,
    icon: PluginPanelIcon,
    minimum_width: f32,
    minimum_height: f32,
    singleton: bool,
    register: fn(&UiPanelRegistration, &mut PluginPanelRegistry) -> Result<(), String>,
}

impl UiPanelRegistration {
    pub const fn panel<P: PluginPanel + Default + 'static>(
        stable_id: &'static str,
        title: &'static str,
    ) -> Self {
        Self {
            stable_id,
            title,
            icon: PluginPanelIcon::Panel,
            minimum_width: 180.0,
            minimum_height: 120.0,
            singleton: false,
            register: register_panel::<P>,
        }
    }

    pub const fn icon(mut self, icon: PluginPanelIcon) -> Self {
        self.icon = icon;
        self
    }

    pub const fn minimum_size(mut self, width: f32, height: f32) -> Self {
        self.minimum_width = width;
        self.minimum_height = height;
        self
    }

    pub const fn singleton(mut self) -> Self {
        self.singleton = true;
        self
    }

    pub const fn stable_id(&self) -> &'static str {
        self.stable_id
    }

    pub const fn title(&self) -> &'static str {
        self.title
    }

    pub(crate) fn apply_to(&self, registry: &mut PluginPanelRegistry) -> Result<(), String> {
        (self.register)(self, registry)
    }

    fn descriptor(&self) -> PluginPanelDescriptor {
        let mut descriptor = PluginPanelDescriptor::new(self.stable_id, self.title)
            .icon(self.icon)
            .minimum_size(self.minimum_width, self.minimum_height);
        if self.singleton {
            descriptor = descriptor.singleton();
        }
        descriptor
    }
}

fn register_panel<P: PluginPanel + Default + 'static>(
    registration: &UiPanelRegistration,
    registry: &mut PluginPanelRegistry,
) -> Result<(), String> {
    registry.register::<P>(registration.descriptor())
}

inventory::collect!(UiPanelRegistration);

pub(crate) fn ui_panel_registrations() -> Vec<&'static UiPanelRegistration> {
    let mut registrations = inventory::iter::<UiPanelRegistration>
        .into_iter()
        .collect::<Vec<_>>();
    validate_ui_panel_registrations(&mut registrations);
    registrations
}

fn validate_ui_panel_registrations(registrations: &mut Vec<&UiPanelRegistration>) {
    registrations.sort_by_key(|registration| registration.stable_id());

    let mut stable_ids = HashSet::new();
    for registration in registrations {
        assert!(
            !registration.stable_id().trim().is_empty(),
            "UI-panel inventory contains an empty stable ID"
        );
        assert!(
            stable_ids.insert(registration.stable_id()),
            "duplicate UI-panel inventory stable ID '{}'",
            registration.stable_id()
        );
        assert!(
            !registration.title().trim().is_empty(),
            "UI-panel inventory feature '{}' has an empty title",
            registration.stable_id()
        );
    }
}

#[cfg(test)]
mod registration_tests {
    use super::super::contract::PluginPanelContext;
    use super::super::registry::PluginPanels;
    use super::*;

    #[derive(Default)]
    struct InventoryPanel;

    impl PluginPanel for InventoryPanel {
        fn show(&mut self, _ui: &mut egui::Ui, _context: PluginPanelContext<'_>) {}
    }

    inventory::submit! {
        UiPanelRegistration::panel::<InventoryPanel>(
            "org.logicconduit.test.inventory-panel/v1",
            "Inventory Panel",
        )
        .icon(PluginPanelIcon::Image)
    }

    #[test]
    fn inventory_panel_is_discovered_and_applied() {
        let registrations = ui_panel_registrations();
        assert!(
            registrations
                .windows(2)
                .all(|pair| pair[0].stable_id() < pair[1].stable_id())
        );

        let panels = PluginPanels::new(PluginPanelRegistry::standard());
        let definition = panels
            .definitions()
            .into_iter()
            .find(|definition| definition.stable_id == "org.logicconduit.test.inventory-panel/v1")
            .expect("inventory panel must be available to application composition");
        assert_eq!(definition.title, "Inventory Panel");
        assert_eq!(definition.icon, PluginPanelIcon::Image);
    }

    #[test]
    fn duplicate_ui_panel_registration_is_rejected() {
        let registration = ui_panel_registrations()[0];
        let mut registrations = vec![registration, registration];

        assert!(
            std::panic::catch_unwind(move || {
                validate_ui_panel_registrations(&mut registrations)
            })
            .is_err()
        );
    }
}
