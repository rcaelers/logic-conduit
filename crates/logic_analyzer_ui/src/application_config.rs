use serde::Deserialize;

use logic_analyzer_viewer::ColorProfile;

std::cfg_select! {
    target_arch = "wasm32" => {
        #[path = "application_config/wasm.rs"]
        mod imp;
    }
    _ => {
        #[path = "application_config/native.rs"]
        mod imp;
    }
}

pub(crate) use imp::{load, path};

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct ApplicationConfig {
    pub(crate) logic_analyzer_viewer: LogicAnalyzerViewerConfig,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct LogicAnalyzerViewerConfig {
    pub(crate) color_profile: ConfiguredColorProfile,
}

impl Default for LogicAnalyzerViewerConfig {
    fn default() -> Self {
        Self {
            color_profile: ConfiguredColorProfile::DsView,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ConfiguredColorProfile {
    DsView,
    Classic,
}

impl From<ConfiguredColorProfile> for ColorProfile {
    fn from(profile: ConfiguredColorProfile) -> Self {
        match profile {
            ConfiguredColorProfile::DsView => Self::DsView,
            ConfiguredColorProfile::Classic => Self::Classic,
        }
    }
}

fn embedded_defaults() -> ApplicationConfig {
    serde_json::from_str(include_str!("../config/application.json"))
        .expect("embedded application configuration must be valid")
}
