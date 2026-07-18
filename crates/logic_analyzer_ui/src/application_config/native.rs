use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use super::ApplicationConfig;

const CONFIG_FILE: &str = "application.json";

pub(crate) fn load() -> ApplicationConfig {
    path().map_or_else(super::embedded_defaults, |path| load_path(&path))
}

pub(crate) fn path() -> Option<PathBuf> {
    dirs::config_dir().map(|directory| directory.join("dsl").join(CONFIG_FILE))
}

fn load_path(path: &Path) -> ApplicationConfig {
    match std::fs::read_to_string(path) {
        Ok(json) => serde_json::from_str(&json).unwrap_or_else(|error| {
            panic!(
                "invalid application configuration in {}: {error}",
                path.display()
            )
        }),
        Err(error) if error.kind() == ErrorKind::NotFound => super::embedded_defaults(),
        Err(error) => panic!(
            "cannot read application configuration from {}: {error}",
            path.display()
        ),
    }
}

#[cfg(test)]
mod tests {
    use logic_analyzer_viewer::ColorProfile;

    use super::{load_path, path};

    #[test]
    fn missing_disk_config_uses_embedded_defaults() {
        let path = std::path::Path::new("definitely-missing-application.json");
        let config = load_path(path);
        assert_eq!(
            ColorProfile::from(config.logic_analyzer_viewer.color_profile),
            ColorProfile::DsView
        );
    }

    #[test]
    fn disk_config_overrides_embedded_defaults() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("application.json");
        std::fs::write(
            &path,
            r#"{
                "logic_analyzer_viewer":{"color_profile":"classic"},
                "live_capture":{"max_recent_sessions":7,"max_storage_gib":12}
            }"#,
        )
        .unwrap();

        let config = load_path(&path);
        assert_eq!(
            ColorProfile::from(config.logic_analyzer_viewer.color_profile),
            ColorProfile::Classic
        );
        assert_eq!(config.live_capture.max_recent_sessions, 7);
        assert_eq!(config.live_capture.max_storage_gib, 12);
    }

    #[test]
    fn standardized_path_has_application_directory_and_file_name() {
        let path = path().expect("this test environment has a user config directory");
        assert!(path.ends_with(std::path::Path::new("dsl/application.json")));
    }
}
