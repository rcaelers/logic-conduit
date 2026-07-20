use super::builtins::FileFilter;

pub(crate) const AVAILABLE: bool = true;

pub(crate) fn pick(title: &str, filters: &[FileFilter], save: bool) -> Option<String> {
    let mut dialog = rfd::FileDialog::new();
    if !title.is_empty() {
        dialog = dialog.set_title(title);
    }
    for filter in filters {
        let extensions: Vec<&str> = filter.extensions.iter().map(String::as_str).collect();
        dialog = dialog.add_filter(&filter.name, &extensions);
    }
    let picked = if save {
        dialog.save_file()
    } else {
        dialog.pick_file()
    };
    picked.map(|path| path.display().to_string())
}
