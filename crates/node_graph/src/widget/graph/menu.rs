use crate::runtime::NodeTypeRegistry;
use egui::Pos2;
use std::collections::HashMap;

use super::super::menu::{MenuEntry, PopupMenu, Shortcut};
use super::action::GraphAction;

// ── Entry builders ────────────────────────────────────────────────────────────

pub(super) fn build_add_entries(
    registry: &NodeTypeRegistry,
    canvas_pos: Pos2,
) -> Vec<MenuEntry<GraphAction>> {
    let mut cats: Vec<String> = Vec::new();
    let mut map: HashMap<String, Vec<String>> = HashMap::new();
    for def in registry.all() {
        map.entry(def.category.clone())
            .or_default()
            .push(def.name.clone());
        if !cats.contains(&def.category) {
            cats.push(def.category.clone());
        }
    }
    let mut entries: Vec<MenuEntry<GraphAction>> = cats
        .into_iter()
        .map(|cat| {
            let names = map.remove(&cat).unwrap_or_default();
            MenuEntry::submenu(
                cat,
                names
                    .into_iter()
                    .map(|name| {
                        let label = name.clone();
                        MenuEntry::action(
                            label,
                            GraphAction::AddNode {
                                name,
                                pos: canvas_pos,
                            },
                        )
                    })
                    .collect(),
            )
        })
        .collect();
    entries.push(MenuEntry::separator());
    entries.push(MenuEntry::action(
        "Reroute",
        GraphAction::AddNode {
            name: "Reroute".to_string(),
            pos: canvas_pos,
        },
    ));
    entries
}

fn paste_entry(canvas_pos: Pos2) -> MenuEntry<GraphAction> {
    MenuEntry::action(
        "Paste",
        GraphAction::Paste {
            text: None,
            pos: canvas_pos,
        },
    )
    .with_icon("▣")
    .with_shortcut(Shortcut::command(egui::Key::V))
}

pub(super) fn build_empty_canvas_entries(
    registry: &NodeTypeRegistry,
    canvas_pos: Pos2,
    can_paste: bool,
    can_undo: bool,
    can_redo: bool,
) -> Vec<MenuEntry<GraphAction>> {
    let mut entries = Vec::new();
    add_undo_redo_entries(&mut entries, can_undo, can_redo);
    if can_paste {
        entries.push(paste_entry(canvas_pos));
        entries.push(MenuEntry::separator());
    }
    entries.push(MenuEntry::submenu("Add", build_add_entries(registry, canvas_pos)).with_icon("+"));
    entries
}

pub(super) fn build_context_entries(
    registry: &NodeTypeRegistry,
    canvas_pos: Pos2,
    context_node: Option<crate::model::NodeId>,
    node_hidden: bool,
    node_collapsed: bool,
    any_selected: bool,
    can_paste: bool,
    can_undo: bool,
    can_redo: bool,
) -> Vec<MenuEntry<GraphAction>> {
    if context_node.is_some() || any_selected {
        let hidden_chk = if node_hidden { "✓  " } else { "    " };
        let collapsed_chk = if node_collapsed { "✓  " } else { "    " };
        let mut entries = Vec::new();
        add_undo_redo_entries(&mut entries, can_undo, can_redo);
        entries.extend([
            MenuEntry::action(
                "Cut",
                GraphAction::Cut {
                    target: context_node,
                },
            )
            .with_icon("✂")
            .with_shortcut(Shortcut::command(egui::Key::X)),
            MenuEntry::action(
                "Copy",
                GraphAction::Copy {
                    target: context_node,
                },
            )
            .with_icon("⧉")
            .with_shortcut(Shortcut::command(egui::Key::C)),
        ]);
        if can_paste {
            entries.push(paste_entry(canvas_pos));
        }
        if any_selected {
            entries.push(
                MenuEntry::action("Duplicate", GraphAction::DuplicateSelected)
                    .with_icon("⧉")
                    .with_shortcut(Shortcut::shift(egui::Key::D)),
            );
        }
        entries.extend([
            MenuEntry::separator(),
            MenuEntry::action(
                "Delete",
                GraphAction::Delete {
                    target: context_node,
                },
            )
            .with_shortcut(Shortcut::key(egui::Key::X)),
            MenuEntry::separator(),
            MenuEntry::action(
                "Join in New Frame",
                GraphAction::AddFrame {
                    target: context_node,
                },
            )
            .with_shortcut(Shortcut::ctrl(egui::Key::J)),
            MenuEntry::action(
                "Remove from Frame",
                GraphAction::RemoveFromFrame {
                    target: context_node,
                },
            ),
            MenuEntry::separator(),
            MenuEntry::submenu(
                "Show/Hide",
                vec![
                    MenuEntry::action(
                        format!("{hidden_chk}Unconnected Sockets"),
                        GraphAction::ToggleHidden {
                            target: context_node,
                        },
                    )
                    .with_shortcut(Shortcut::ctrl(egui::Key::H)),
                    MenuEntry::action(
                        format!("{collapsed_chk}Collapse"),
                        GraphAction::ToggleCollapsed {
                            target: context_node,
                        },
                    ),
                ],
            ),
        ]);
        entries
    } else {
        build_empty_canvas_entries(registry, canvas_pos, can_paste, can_undo, can_redo)
    }
}

fn add_undo_redo_entries(
    entries: &mut Vec<MenuEntry<GraphAction>>,
    can_undo: bool,
    can_redo: bool,
) {
    if can_undo {
        entries.push(
            MenuEntry::action("Undo", GraphAction::Undo)
                .with_icon("↶")
                .with_shortcut(Shortcut::command(egui::Key::Z)),
        );
    }
    if can_redo {
        entries.push(
            MenuEntry::action("Redo", GraphAction::Redo)
                .with_icon("↷")
                .with_shortcut(Shortcut::command_shift(egui::Key::Z)),
        );
    }
    if can_undo || can_redo {
        entries.push(MenuEntry::separator());
    }
}

// ── Controller ────────────────────────────────────────────────────────────────

pub(super) struct MenuController {
    popup: PopupMenu<GraphAction>,
    sec_press: Option<Pos2>,
}

impl MenuController {
    pub fn new() -> Self {
        Self {
            popup: PopupMenu::new(egui::Id::new("dsl_add_node_popup")),
            sec_press: None,
        }
    }

    /// Call when `response.context_menu_opened()` is true.
    /// `entries` must already carry the canvas position in each add-node action.
    pub fn on_context_opened(&mut self, entries: Vec<MenuEntry<GraphAction>>) {
        self.popup.set_context_entries(entries);
    }

    /// Open the standalone add-node popup (e.g. Shift+A).
    pub fn open_popup(&mut self, screen_pos: Pos2, entries: Vec<MenuEntry<GraphAction>>) {
        self.popup.open_popup(screen_pos, entries);
    }

    /// Drive for one frame: tablet gesture detection, keyboard nav, rendering.
    /// `allow`: false while wire-cutting is active.
    pub fn update(
        &mut self,
        ui: &mut egui::Ui,
        response: &egui::Response,
        pointer: Option<Pos2>,
        allow: bool,
    ) -> Option<GraphAction> {
        // Track secondary press start for tablet long-press gesture.
        if ui.input(|i| i.pointer.button_pressed(egui::PointerButton::Secondary)) {
            self.sec_press = pointer;
        }

        // Tablet: synthesise context-menu open on secondary release within threshold.
        let released = ui.input(|i| i.pointer.button_released(egui::PointerButton::Secondary));
        if released
            && allow
            && !response.secondary_clicked()
            && !ui.input(|i| i.modifiers.ctrl)
            && let Some(press) = self.sec_press
            && let Some(curr) = pointer
            && press.distance(curr) < 30.0
        {
            let popup_id = egui::Popup::default_response_id(response);
            #[allow(deprecated)]
            ui.ctx().memory_mut(|mem| mem.open_popup_at(popup_id, curr));
        }
        if released {
            self.sec_press = None;
        }

        let kb = self.popup.handle_keys(ui, response);
        let mouse = self.popup.render(ui, response);
        kb.or(mouse)
    }
}
