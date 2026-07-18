use std::collections::HashSet;
use std::path::PathBuf;

use node_graph::NodeId;

pub(crate) enum FileCommand {
    New,
    Load,
    LoadPath(PathBuf),
    ClearRecent,
    Save,
    SaveAs,
    ExportDslCapture,
    ExportPortableCapture,
    Quit,
}

pub(crate) enum GuardedAction {
    Quit,
    New,
    LoadPath(PathBuf),
}

const MAX_RECENT_FILES: usize = 10;

#[derive(serde::Serialize, serde::Deserialize)]
struct PersistedState {
    analyzer_split: f32,
    #[serde(default)]
    panel_layout: Option<panel_layout::PanelLayoutState>,
    graph_ui_prefs: node_graph::GraphUiPrefs,
    recent_files: Vec<PathBuf>,
}

fn normalize_recent_files(paths: impl IntoIterator<Item = PathBuf>) -> Vec<PathBuf> {
    let mut seen = HashSet::new();
    let mut result = Vec::new();
    for path in paths {
        let canonical = path.canonicalize().unwrap_or(path);
        if seen.insert(canonical.clone()) {
            result.push(canonical);
        }
        if result.len() >= MAX_RECENT_FILES {
            break;
        }
    }
    result
}

pub(crate) struct PlatformState {
    pub(crate) current_file: Option<PathBuf>,
    pub(crate) saved_graph: serde_json::Value,
    pub(crate) pending_guarded_action: Option<GuardedAction>,
    pub(crate) allow_close: bool,
    pub(crate) recent_files: Vec<PathBuf>,
    pub(crate) confirm_clear_recent: bool,
    pub(crate) confirm_clear_derived_caches: bool,
    pub(crate) derived_cache_nodes: HashSet<NodeId>,
    pub(crate) preview_source: Option<NodeId>,
    #[cfg(target_os = "macos")]
    pub(crate) native_menu_commands: crossbeam_channel::Receiver<NativeMenuCommand>,
}

#[cfg(target_os = "macos")]
#[derive(Clone)]
pub enum NativeMenuCommand {
    About,
    New,
    Load,
    LoadPath(PathBuf),
    ClearRecent,
    Save,
    SaveAs,
    ExportDslCapture,
    ExportPortableCapture,
    Quit,
    Run,
    Stop,
    ClearDerivedCaches,
    ShowWatches,
    ShowTriggers,
    ShowDecoder,
    ResetLayout,
}

#[cfg(target_os = "macos")]
struct NativeMenuBridge {
    sender: crossbeam_channel::Sender<NativeMenuCommand>,
    context: egui::Context,
}

#[cfg(target_os = "macos")]
static NATIVE_MENU_BRIDGE: std::sync::OnceLock<NativeMenuBridge> = std::sync::OnceLock::new();

#[cfg(target_os = "macos")]
pub fn dispatch_native_menu_command(command: NativeMenuCommand) {
    if let Some(bridge) = NATIVE_MENU_BRIDGE.get() {
        let _ = bridge.sender.send(command);
        bridge.context.request_repaint();
    }
}

#[cfg(target_os = "macos")]
type RecentFilesListener = Box<dyn Fn(&[PathBuf]) + Send + Sync>;

#[cfg(target_os = "macos")]
static RECENT_FILES_LISTENER: std::sync::OnceLock<RecentFilesListener> = std::sync::OnceLock::new();

#[cfg(target_os = "macos")]
pub fn set_recent_files_listener(listener: impl Fn(&[PathBuf]) + Send + Sync + 'static) {
    let _ = RECENT_FILES_LISTENER.set(Box::new(listener));
}

#[cfg(target_os = "macos")]
pub(crate) fn notify_recent_files_changed(paths: &[PathBuf]) {
    if let Some(listener) = RECENT_FILES_LISTENER.get() {
        listener(paths);
    }
}

impl PlatformState {
    pub(crate) fn restore(
        cc: &eframe::CreationContext,
        widget: &mut node_graph::NodeGraphWidget,
    ) -> (Self, Option<panel_layout::PanelLayoutState>, f32) {
        let persisted = cc
            .storage
            .and_then(|storage| eframe::get_value::<PersistedState>(storage, eframe::APP_KEY));
        if let Some(persisted) = &persisted {
            widget.set_ui_prefs(persisted.graph_ui_prefs.clone());
        }
        let analyzer_split = persisted
            .as_ref()
            .map_or(0.42, |state| state.analyzer_split);
        let panel_layout = persisted
            .as_ref()
            .and_then(|state| state.panel_layout.clone());
        let recent_files = persisted
            .map(|state| normalize_recent_files(state.recent_files))
            .unwrap_or_default();
        let saved_graph = widget
            .snapshot_value()
            .expect("new graph should always serialize");
        #[cfg(target_os = "macos")]
        let native_menu_commands = {
            let (sender, receiver) = crossbeam_channel::unbounded();
            assert!(
                NATIVE_MENU_BRIDGE
                    .set(NativeMenuBridge {
                        sender,
                        context: cc.egui_ctx.clone(),
                    })
                    .is_ok(),
                "only one native application instance is supported"
            );
            receiver
        };
        (
            Self {
                current_file: None,
                saved_graph,
                pending_guarded_action: None,
                allow_close: false,
                recent_files,
                confirm_clear_recent: false,
                confirm_clear_derived_caches: false,
                derived_cache_nodes: HashSet::new(),
                preview_source: None,
                #[cfg(target_os = "macos")]
                native_menu_commands,
            },
            panel_layout,
            analyzer_split,
        )
    }

    pub(crate) fn recent_files(&self) -> &[PathBuf] {
        &self.recent_files
    }

    pub(crate) fn push_recent_file(&mut self, path: PathBuf) {
        let mut paths = self.recent_files.clone();
        paths.insert(0, path);
        self.recent_files = normalize_recent_files(paths);
    }

    pub(crate) fn save(
        &self,
        storage: &mut dyn eframe::Storage,
        analyzer_split: f32,
        panel_layout: panel_layout::PanelLayoutState,
        graph_ui_prefs: node_graph::GraphUiPrefs,
    ) {
        let state = PersistedState {
            analyzer_split,
            panel_layout: Some(panel_layout),
            graph_ui_prefs,
            recent_files: self.recent_files.clone(),
        };
        eframe::set_value(storage, eframe::APP_KEY, &state);
    }
}

#[cfg(test)]
mod tests {
    use super::PersistedState;

    #[test]
    fn legacy_state_without_panel_tree_still_loads() {
        let legacy = serde_json::json!({
            "analyzer_split": 0.37,
            "graph_ui_prefs": {
                "panel_width": 320.0,
                "panel_tab": null,
                "minimap_visible": true,
            },
            "recent_files": [],
        });
        let restored: PersistedState = serde_json::from_value(legacy).unwrap();
        assert_eq!(restored.analyzer_split, 0.37);
        assert!(restored.panel_layout.is_none());
    }
}
