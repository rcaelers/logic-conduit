//! Public contracts implemented by compile-time plugin panels.

use signal_processing::OpaqueCollectedLane;

/// Read-only application data exposed while a plugin panel is drawn.
pub struct PluginPanelContext<'a> {
    lanes: &'a [OpaqueCollectedLane],
}

impl<'a> PluginPanelContext<'a> {
    pub(crate) fn new(lanes: &'a [OpaqueCollectedLane]) -> Self {
        Self { lanes }
    }

    pub fn collected_lanes(&self) -> &'a [OpaqueCollectedLane] {
        self.lanes
    }
}

/// One independently persisted panel instance.
pub trait PluginPanel: Send {
    fn show(&mut self, ui: &mut egui::Ui, context: PluginPanelContext<'_>);

    fn save_state(&self) -> serde_json::Value {
        serde_json::Value::Null
    }

    fn restore_state(&mut self, _state: serde_json::Value) -> Result<(), String> {
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PluginPanelIcon {
    #[default]
    Panel,
    Image,
    List,
    Table,
}

/// Runtime registration metadata built from an inventory submission.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PluginPanelDescriptor {
    pub(crate) stable_id: String,
    pub(crate) title: String,
    pub(crate) icon: PluginPanelIcon,
    pub(crate) minimum_width: f32,
    pub(crate) minimum_height: f32,
    pub(crate) singleton: bool,
}

impl PluginPanelDescriptor {
    pub(crate) fn new(stable_id: impl Into<String>, title: impl Into<String>) -> Self {
        Self {
            stable_id: stable_id.into(),
            title: title.into(),
            icon: PluginPanelIcon::Panel,
            minimum_width: 180.0,
            minimum_height: 120.0,
            singleton: false,
        }
    }

    pub(crate) fn icon(mut self, icon: PluginPanelIcon) -> Self {
        self.icon = icon;
        self
    }

    pub(crate) fn minimum_size(mut self, width: f32, height: f32) -> Self {
        self.minimum_width = width;
        self.minimum_height = height;
        self
    }

    pub(crate) fn singleton(mut self) -> Self {
        self.singleton = true;
        self
    }
}
