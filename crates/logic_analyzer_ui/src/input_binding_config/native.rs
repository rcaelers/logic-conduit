use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use input_bindings::InputBindings;

use super::implementation::embedded_defaults;
use crate::product::APPLICATION_ID;

const CONFIG_FILE: &str = "input_bindings.json";

pub(crate) fn load() -> InputBindings {
    path().map_or_else(embedded_defaults, |path| load_path(&path))
}

fn path() -> Option<PathBuf> {
    dirs::config_dir().map(|directory| path_in(&directory))
}

fn path_in(directory: &Path) -> PathBuf {
    directory.join(APPLICATION_ID).join(CONFIG_FILE)
}

fn load_path(path: &Path) -> InputBindings {
    match std::fs::read_to_string(path) {
        Ok(json) => InputBindings::from_json(&json).unwrap_or_else(|error| {
            panic!("invalid input bindings in {}: {error}", path.display())
        }),
        Err(error) if error.kind() == ErrorKind::NotFound => embedded_defaults(),
        Err(error) => panic!(
            "cannot read input bindings from {}: {error}",
            path.display()
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::{load_path, path_in};

    #[test]
    fn missing_disk_config_uses_embedded_defaults() {
        let path = std::path::Path::new("definitely-missing-input-bindings.json");
        let bindings = load_path(path);
        assert!(bindings.shortcut(&["global"], "save").is_some());
    }

    #[test]
    fn disk_config_overrides_embedded_defaults() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("input_bindings.json");
        std::fs::write(
            &path,
            r#"{"bindings":[
              {"context":"custom","action":"only","label":"Only","input":"key","key":"f12"}
            ]}"#,
        )
        .unwrap();

        let bindings = load_path(&path);
        assert!(bindings.shortcut(&["custom"], "only").is_some());
        assert!(bindings.shortcut(&["global"], "save").is_none());
    }

    #[test]
    fn standardized_path_has_application_directory_and_file_name() {
        let directory = tempfile::tempdir().unwrap();
        let path = path_in(directory.path());
        assert!(path.ends_with(std::path::Path::new("logic-conduit/input_bindings.json")));
    }
}
