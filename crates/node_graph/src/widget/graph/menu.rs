use crate::{model::FrameId, runtime::NodeTypeRegistry};
use egui::{Color32, Pos2};
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
    screen_pos: Pos2,
    context_node: Option<crate::model::NodeId>,
    context_frame: Option<FrameId>,
    any_frame_selected: bool,
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
            .with_icon("🗐")
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
        if context_frame.is_some() || any_frame_selected {
            let mut frame_entries = Vec::new();
            if let Some(frame_id) = context_frame {
                frame_entries.push(MenuEntry::action(
                    "Rename",
                    GraphAction::RenameFrame {
                        target: frame_id,
                        screen_pos,
                    },
                ));
            }
            frame_entries.push(MenuEntry::submenu(
                "Color",
                vec![MenuEntry::palette(frame_color_palette(context_frame))],
            ));
            entries.push(MenuEntry::separator());
            entries.push(MenuEntry::submenu("Frame", frame_entries));
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
            MenuEntry::action(
                "Dissolve",
                GraphAction::Dissolve {
                    target: context_node,
                },
            ),
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

fn frame_color_palette(target: Option<FrameId>) -> Vec<(Color32, GraphAction)> {
    const BASE: [(u8, u8, u8); 8] = [
        (190, 55, 55),
        (200, 120, 45),
        (190, 165, 45),
        (55, 145, 75),
        (45, 160, 165),
        (55, 105, 190),
        (125, 80, 190),
        (170, 170, 170),
    ];
    const MIX: [(u8, u8); 8] = [
        (45, 0),
        (25, 0),
        (10, 0),
        (0, 0),
        (0, 25),
        (0, 45),
        (0, 65),
        (0, 82),
    ];

    MIX.into_iter()
        .flat_map(|(black, white)| {
            BASE.into_iter().map(move |(r, g, b)| {
                let color = mix_frame_color(r, g, b, black, white);
                (color, GraphAction::SetFrameColor { target, color })
            })
        })
        .collect()
}

fn mix_frame_color(r: u8, g: u8, b: u8, black: u8, white: u8) -> Color32 {
    let scale = 100_u16 - black as u16;
    let white = white as u16;
    let mix = |channel: u8| -> u8 {
        let darkened = channel as u16 * scale / 100;
        (darkened + (255 - darkened) * white / 100).min(255) as u8
    };
    Color32::from_rgb(mix(r), mix(g), mix(b))
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

const TABLET_CONTEXT_DRIFT_THRESHOLD: f32 = 72.0;

pub(super) struct MenuController {
    popup: PopupMenu<GraphAction>,
    secondary_press: Option<Pos2>,
}

impl MenuController {
    pub fn new() -> Self {
        Self {
            popup: PopupMenu::new(egui::Id::new("dsl_add_node_popup")),
            secondary_press: None,
        }
    }

    /// Open the standalone add-node popup (e.g. Shift+A).
    pub fn open_popup(&mut self, screen_pos: Pos2, entries: Vec<MenuEntry<GraphAction>>) {
        self.popup.open_popup(screen_pos, entries);
    }

    pub fn context_trigger_pos(
        &mut self,
        ui: &egui::Ui,
        pointer: Option<Pos2>,
        allow: bool,
    ) -> Option<Pos2> {
        if self.popup.is_open() {
            if ui.input(|i| i.pointer.button_pressed(egui::PointerButton::Secondary))
                || ui.input(|i| i.pointer.button_released(egui::PointerButton::Secondary))
            {
                self.secondary_press = None;
            }
            return None;
        }

        if ui.input(|i| i.pointer.button_pressed(egui::PointerButton::Secondary)) {
            self.secondary_press = pointer;
        }

        if !ui.input(|i| i.pointer.button_released(egui::PointerButton::Secondary)) {
            return None;
        }

        let press = self.secondary_press.take()?;
        if !allow || ui.input(|i| i.modifiers.ctrl) {
            return None;
        }
        let release = pointer.unwrap_or(press);
        (press.distance(release) <= TABLET_CONTEXT_DRIFT_THRESHOLD).then_some(press)
    }

    /// Drive for one frame: tablet gesture detection, keyboard nav, rendering.
    /// `allow`: false while wire-cutting is active.
    pub fn update(
        &mut self,
        ui: &mut egui::Ui,
        response: &egui::Response,
        _pointer: Option<Pos2>,
        _allow: bool,
    ) -> Option<GraphAction> {
        let kb = self.popup.handle_keys(ui, response);
        let mouse = self.popup.render(ui, response);
        kb.or(mouse)
    }
}
