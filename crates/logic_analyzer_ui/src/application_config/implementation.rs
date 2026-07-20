use serde::Deserialize;

use logic_analyzer_viewer::ColorProfile;

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct ApplicationConfig {
    pub(crate) logic_analyzer_viewer: LogicAnalyzerViewerConfig,
    pub(crate) live_capture: LiveCaptureConfig,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct LiveCaptureConfig {
    pub(crate) max_recent_sessions: usize,
    pub(crate) max_storage_gib: u64,
}

impl Default for LiveCaptureConfig {
    fn default() -> Self {
        Self {
            max_recent_sessions: 10,
            max_storage_gib: 20,
        }
    }
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

pub(crate) fn embedded_defaults() -> ApplicationConfig {
    serde_json::from_str(include_str!("../../config/application.json"))
        .expect("embedded application configuration must be valid")
}
