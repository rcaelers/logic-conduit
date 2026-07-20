use std::collections::HashMap;

use egui::{Color32, Pos2};

use input_bindings::InputBindings;

use super::super::menu::{MenuEntry, PopupMenu, Shortcut};
use super::action::GraphAction;
use super::widget::NodeContextAction;
use crate::model::{FrameId, Socket, SocketId};
use crate::runtime::NodeTypeRegistry;

// ── Entry builders ────────────────────────────────────────────────────────────

#[derive(Clone)]
struct AddSearchItem {
    label: String,
    action: GraphAction,
}

fn build_add_popup_entries(
    registry: &NodeTypeRegistry,
    canvas_pos: Pos2,
) -> Vec<MenuEntry<GraphAction>> {
    let mut entries = vec![
        MenuEntry::action("Search...", GraphAction::OpenAddSearch).with_icon("🔍"),
        MenuEntry::separator(),
    ];
    entries.extend(build_add_entries(registry, canvas_pos));
    entries
}

fn build_add_search_items(registry: &NodeTypeRegistry, canvas_pos: Pos2) -> Vec<AddSearchItem> {
    let mut items: Vec<AddSearchItem> = registry
        .all()
        .iter()
        .map(|def| AddSearchItem {
            label: format!("{} → {}", def.category, def.name),
            action: GraphAction::AddNode {
                name: def.name.clone(),
                pos: canvas_pos,
            },
        })
        .collect();
    items.push(AddSearchItem {
        label: "Reroute".to_string(),
        action: GraphAction::AddNode {
            name: "Reroute".to_string(),
            pos: canvas_pos,
        },
    });
    items
}

/// Search items for the link-drag-search popup: only node types with a
/// visible socket compatible with `from` (plus Reroute, which is always
/// compatible since both its sockets are `Any`). Picking one both adds the
/// node and wires it to `from` in a single undo step.
fn build_link_drag_search_items(
    registry: &NodeTypeRegistry,
    canvas_pos: Pos2,
    from: SocketId,
    from_socket: &Socket,
) -> Vec<AddSearchItem> {
    let mut items: Vec<AddSearchItem> = registry
        .connectable_types(from_socket, from.direction)
        .into_iter()
        .map(|def| AddSearchItem {
            label: format!("{} → {}", def.category, def.name),
            action: GraphAction::AddNodeAndConnect {
                name: def.name.clone(),
                pos: canvas_pos,
                from,
            },
        })
        .collect();
    items.push(AddSearchItem {
        label: "Reroute".to_string(),
        action: GraphAction::AddNodeAndConnect {
            name: "Reroute".to_string(),
            pos: canvas_pos,
            from,
        },
    });
    items
}

fn build_add_entries(registry: &NodeTypeRegistry, canvas_pos: Pos2) -> Vec<MenuEntry<GraphAction>> {
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

fn paste_entry(canvas_pos: Pos2, input_bindings: &InputBindings) -> MenuEntry<GraphAction> {
    configured_shortcut(
        MenuEntry::action(
            "Paste",
            GraphAction::Paste {
                text: None,
                pos: canvas_pos,
            },
        )
        .with_icon("▣"),
        input_bindings,
        "paste",
    )
}

fn build_empty_canvas_entries(
    registry: &NodeTypeRegistry,
    canvas_pos: Pos2,
    can_paste: bool,
    can_undo: bool,
    can_redo: bool,
    input_bindings: &InputBindings,
) -> Vec<MenuEntry<GraphAction>> {
    let mut entries = Vec::new();
    add_undo_redo_entries(&mut entries, can_undo, can_redo, input_bindings);
    if can_paste {
        entries.push(paste_entry(canvas_pos, input_bindings));
        entries.push(MenuEntry::separator());
    }
    entries.push(MenuEntry::submenu("Add", build_add_entries(registry, canvas_pos)).with_icon("+"));
    entries
}

pub(crate) struct ContextMenuState<'a> {
    pub registry: &'a NodeTypeRegistry,
    pub canvas_pos: Pos2,
    pub screen_pos: Pos2,
    pub context_node: Option<crate::model::NodeId>,
    pub context_frame: Option<FrameId>,
    pub any_frame_selected: bool,
    pub node_hidden: bool,
    pub node_collapsed: bool,
    pub node_muted: bool,
    pub node_has_derived_cache: bool,
    pub node_actions: &'a [NodeContextAction],
    pub any_selected: bool,
    pub can_paste: bool,
    pub can_undo: bool,
    pub can_redo: bool,
    pub editing_enabled: bool,
    pub input_bindings: &'a InputBindings,
}

pub(crate) fn build_context_entries(context: ContextMenuState<'_>) -> Vec<MenuEntry<GraphAction>> {
    let ContextMenuState {
        registry,
        canvas_pos,
        screen_pos,
        context_node,
        context_frame,
        any_frame_selected,
        node_hidden,
        node_collapsed,
        node_muted,
        node_has_derived_cache,
        node_actions,
        any_selected,
        can_paste,
        can_undo,
        can_redo,
        editing_enabled,
        input_bindings,
    } = context;
    if !editing_enabled {
        return if context_node.is_some() || any_selected {
            vec![configured_shortcut(
                MenuEntry::action(
                    "Copy",
                    GraphAction::Copy {
                        target: context_node,
                    },
                )
                .with_icon("🗐"),
                input_bindings,
                "copy",
            )]
        } else {
            Vec::new()
        };
    }
    if context_node.is_some() || any_selected {
        let mut entries = Vec::new();
        add_undo_redo_entries(&mut entries, can_undo, can_redo, input_bindings);
        entries.extend([
            configured_shortcut(
                MenuEntry::action(
                    "Cut",
                    GraphAction::Cut {
                        target: context_node,
                    },
                )
                .with_icon("✂"),
                input_bindings,
                "cut",
            ),
            configured_shortcut(
                MenuEntry::action(
                    "Copy",
                    GraphAction::Copy {
                        target: context_node,
                    },
                )
                .with_icon("🗐"),
                input_bindings,
                "copy",
            ),
        ]);
        if can_paste {
            entries.push(paste_entry(canvas_pos, input_bindings));
        }
        if any_selected {
            entries.push(configured_shortcut(
                MenuEntry::action("Duplicate", GraphAction::DuplicateSelected).with_icon("⧉"),
                input_bindings,
                "duplicate",
            ));
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
            configured_shortcut(
                MenuEntry::action(
                    "Mute",
                    GraphAction::ToggleMuted {
                        target: context_node,
                    },
                )
                .with_checkmark(node_muted),
                input_bindings,
                "mute",
            ),
        ]);
        if let Some(target) = context_node {
            for action in node_actions {
                let mut entry = MenuEntry::action(
                    action.label.clone(),
                    GraphAction::HostNodeAction {
                        target,
                        action_id: action.id.clone(),
                    },
                )
                .with_checkmark(action.checked);
                if let Some(icon) = &action.icon {
                    entry = entry.with_icon(icon.clone());
                }
                entries.push(entry);
            }
        }
        if node_has_derived_cache && let Some(target) = context_node {
            entries.extend([
                MenuEntry::separator(),
                MenuEntry::action(
                    "Clear Derived Data Cache",
                    GraphAction::ClearDerivedCache { target },
                ),
            ]);
        }
        entries.extend([
            MenuEntry::separator(),
            configured_shortcut(
                MenuEntry::action(
                    "Delete",
                    GraphAction::Delete {
                        target: context_node,
                    },
                ),
                input_bindings,
                "delete",
            ),
            MenuEntry::action(
                "Dissolve",
                GraphAction::Dissolve {
                    target: context_node,
                },
            ),
            MenuEntry::separator(),
            configured_shortcut(
                MenuEntry::action(
                    "Join in New Frame",
                    GraphAction::AddFrame {
                        target: context_node,
                    },
                ),
                input_bindings,
                "join_frame",
            ),
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
                    configured_shortcut(
                        MenuEntry::action(
                            "Unconnected Sockets",
                            GraphAction::ToggleHidden {
                                target: context_node,
                            },
                        )
                        .with_checkmark(node_hidden),
                        input_bindings,
                        "toggle_unconnected",
                    ),
                    configured_shortcut(
                        MenuEntry::action(
                            "Collapse",
                            GraphAction::ToggleCollapsed {
                                target: context_node,
                            },
                        )
                        .with_checkmark(node_collapsed),
                        input_bindings,
                        "toggle_collapsed",
                    ),
                ],
            ),
        ]);
        entries
    } else {
        build_empty_canvas_entries(
            registry,
            canvas_pos,
            can_paste,
            can_undo,
            can_redo,
            input_bindings,
        )
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
    input_bindings: &InputBindings,
) {
    if can_undo {
        entries.push(configured_shortcut(
            MenuEntry::action("Undo", GraphAction::Undo).with_icon("↶"),
            input_bindings,
            "undo",
        ));
    }
    if can_redo {
        entries.push(configured_shortcut(
            MenuEntry::action("Redo", GraphAction::Redo).with_icon("↷"),
            input_bindings,
            "redo",
        ));
    }
    if can_undo || can_redo {
        entries.push(MenuEntry::separator());
    }
}

fn configured_shortcut<T>(
    entry: MenuEntry<T>,
    input_bindings: &InputBindings,
    action: &str,
) -> MenuEntry<T> {
    if let Some(shortcut) = input_bindings.shortcut(&["node_graph"], action) {
        entry.with_shortcut(Shortcut::from_keyboard(shortcut))
    } else {
        entry
    }
}

// ── Controller ────────────────────────────────────────────────────────────────

const TABLET_CONTEXT_DRIFT_THRESHOLD: f32 = 72.0;
const ADD_SEARCH_RESULTS_HEIGHT: f32 = 320.0;

struct AddSearchPopup {
    visible: bool,
    pos: Pos2,
    rect: Option<egui::Rect>,
    query: String,
    items: Vec<AddSearchItem>,
    focus_requested: bool,
}

impl AddSearchPopup {
    const TEXT_ID: &'static str = "node_graph_add_search_text";

    fn new() -> Self {
        Self {
            visible: false,
            pos: Pos2::ZERO,
            rect: None,
            query: String::new(),
            items: Vec::new(),
            focus_requested: false,
        }
    }

    fn open(&mut self, pos: Pos2, items: Vec<AddSearchItem>) {
        self.visible = true;
        self.pos = pos;
        self.rect = None;
        self.query.clear();
        self.items = items;
        self.focus_requested = false;
    }

    fn close(&mut self) {
        self.visible = false;
        self.rect = None;
        self.query.clear();
        self.items.clear();
        self.focus_requested = false;
    }

    fn render(&mut self, ui: &mut egui::Ui) -> Option<GraphAction> {
        if !self.visible {
            return None;
        }
        let escape = ui.input(|input| input.key_pressed(egui::Key::Escape));
        let sec_press =
            ui.input(|input| input.pointer.button_pressed(egui::PointerButton::Secondary));
        if escape || sec_press {
            self.close();
            return None;
        }

        let text_id = egui::Id::new(Self::TEXT_ID);
        let mut result = None;
        let mut first_match: Option<GraphAction> = None;
        let area_response = egui::Area::new(egui::Id::new("node_graph_add_search_popup"))
            .fixed_pos(self.pos)
            .order(egui::Order::Foreground)
            .show(ui.ctx(), |ui| {
                egui::Frame::menu(ui.style()).show(ui, |ui| {
                    ui.set_min_width(300.0);
                    ui.add_space(2.0);
                    ui.label(egui::RichText::new("Add").strong());
                    ui.separator();
                    ui.horizontal(|ui| {
                        ui.label("🔍");
                        let response = ui.add(
                            egui::TextEdit::singleline(&mut self.query)
                                .id(text_id)
                                .desired_width(250.0)
                                .hint_text("Search"),
                        );
                        if !self.focus_requested {
                            response.request_focus();
                            self.focus_requested = true;
                        }
                    });
                    ui.separator();

                    let query = self.query.trim().to_ascii_lowercase();
                    egui::ScrollArea::vertical()
                        .max_height(ADD_SEARCH_RESULTS_HEIGHT)
                        .min_scrolled_height(ADD_SEARCH_RESULTS_HEIGHT)
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            let mut any_match = false;
                            for item in self.items.iter().filter(|item| {
                                query.is_empty() || item.label.to_ascii_lowercase().contains(&query)
                            }) {
                                any_match = true;
                                if first_match.is_none() {
                                    first_match = Some(item.action.clone());
                                }
                                let clicked = ui
                                    .push_id(("add-search-result", &item.label), |ui| {
                                        Self::search_item_row(ui, &item.label).clicked()
                                    })
                                    .inner;
                                if clicked {
                                    result = Some(item.action.clone());
                                }
                            }
                            if !any_match {
                                ui.add_space(4.0);
                                ui.label(egui::RichText::new("No compatible nodes").weak());
                            }
                        });
                });
            });
        self.rect = Some(area_response.response.rect);

        if result.is_none() && ui.input(|input| input.key_pressed(egui::Key::Enter)) {
            result = first_match;
        }

        if result.is_some() {
            self.close();
            return result;
        }
        let clicked_outside = ui.input(|input| {
            input.pointer.button_released(egui::PointerButton::Primary)
                && input
                    .pointer
                    .latest_pos()
                    .is_some_and(|pos| !area_response.response.rect.expand(2.0).contains(pos))
        });
        if clicked_outside {
            self.close();
        }
        None
    }

    fn blocks_canvas_scroll(&self, ui: &egui::Ui) -> bool {
        self.visible
            && self.rect.is_some_and(|rect| {
                ui.input(|i| {
                    i.pointer
                        .hover_pos()
                        .is_some_and(|pos| rect.expand(2.0).contains(pos))
                })
            })
    }

    fn search_item_row(ui: &mut egui::Ui, label: &str) -> egui::Response {
        let row_height = ui.spacing().interact_size.y;
        let (rect, response) = ui.allocate_exact_size(
            egui::vec2(ui.available_width(), row_height),
            egui::Sense::click(),
        );
        let name_color = if response.hovered() {
            ui.visuals().widgets.hovered.fg_stroke.color
        } else {
            ui.visuals().widgets.inactive.fg_stroke.color
        };
        let path_color = ui.visuals().weak_text_color();
        let font_id = egui::TextStyle::Button.resolve(ui.style());
        let mut job = egui::text::LayoutJob::default();
        if let Some((path, name)) = label.rsplit_once('→') {
            job.append(
                path.trim_end(),
                0.0,
                egui::TextFormat {
                    font_id: font_id.clone(),
                    color: path_color,
                    ..Default::default()
                },
            );
            job.append(
                " → ",
                0.0,
                egui::TextFormat {
                    font_id: font_id.clone(),
                    color: path_color,
                    ..Default::default()
                },
            );
            job.append(
                name.trim_start(),
                0.0,
                egui::TextFormat {
                    font_id,
                    color: name_color,
                    ..Default::default()
                },
            );
        } else {
            job.append(
                label,
                0.0,
                egui::TextFormat {
                    font_id,
                    color: name_color,
                    ..Default::default()
                },
            );
        }
        let galley = ui.painter().layout_job(job);
        let text_pos = egui::pos2(rect.left(), rect.center().y - galley.size().y * 0.5);
        ui.painter().galley(text_pos, galley, name_color);
        response
    }
}

pub(crate) struct MenuController {
    popup: PopupMenu<GraphAction>,
    add_search: AddSearchPopup,
    add_popup_pos: Pos2,
    add_search_items: Vec<AddSearchItem>,
    secondary_press: Option<Pos2>,
}

impl MenuController {
    pub(crate) fn new() -> Self {
        Self {
            popup: PopupMenu::new(egui::Id::new("dsl_add_node_popup")),
            add_search: AddSearchPopup::new(),
            add_popup_pos: Pos2::ZERO,
            add_search_items: Vec::new(),
            secondary_press: None,
        }
    }

    /// Open the standalone add-node popup (e.g. Shift+A).
    pub(crate) fn open_popup(&mut self, screen_pos: Pos2, entries: Vec<MenuEntry<GraphAction>>) {
        self.add_search.close();
        self.popup.open_popup(screen_pos, entries);
    }

    pub(crate) fn open_add_popup(
        &mut self,
        screen_pos: Pos2,
        registry: &NodeTypeRegistry,
        canvas_pos: Pos2,
    ) {
        self.add_search.close();
        self.add_popup_pos = screen_pos;
        self.add_search_items = build_add_search_items(registry, canvas_pos);
        self.popup.open_popup_with_title(
            screen_pos,
            "Add",
            build_add_popup_entries(registry, canvas_pos),
        );
    }

    /// Opens the search box directly (skipping the intermediate "Add" popup)
    /// filtered to node types compatible with `from` — the link-drag-search
    /// gesture triggered by releasing a dragged wire over empty canvas.
    pub(crate) fn open_link_drag_search(
        &mut self,
        screen_pos: Pos2,
        registry: &NodeTypeRegistry,
        canvas_pos: Pos2,
        from: SocketId,
        from_socket: &Socket,
    ) {
        let items = build_link_drag_search_items(registry, canvas_pos, from, from_socket);
        self.add_search.open(screen_pos, items);
    }

    pub(crate) fn context_trigger_pos(
        &mut self,
        ui: &egui::Ui,
        pointer: Option<Pos2>,
        allow: bool,
        input_bindings: &InputBindings,
    ) -> Option<Pos2> {
        let modifiers = ui.input(|input| input.modifiers);
        let context_button = input_bindings
            .pointer_trigger(&["node_graph"], "options", modifiers)
            .map(|(button, _)| button)?;
        if self.popup.is_open() || self.add_search.visible {
            if ui.input(|i| i.pointer.button_pressed(context_button))
                || ui.input(|i| i.pointer.button_released(context_button))
            {
                self.secondary_press = None;
            }
            return None;
        }

        if ui.input(|i| i.pointer.button_pressed(context_button)) {
            self.secondary_press = pointer;
        }

        if !ui.input(|i| i.pointer.button_released(context_button)) {
            return None;
        }

        let press = self.secondary_press.take()?;
        if !allow {
            return None;
        }
        let release = pointer.unwrap_or(press);
        (press.distance(release) <= TABLET_CONTEXT_DRIFT_THRESHOLD).then_some(press)
    }

    pub(crate) fn blocks_canvas_scroll(&self, ui: &egui::Ui) -> bool {
        self.add_search.blocks_canvas_scroll(ui)
    }

    /// Drive for one frame: tablet gesture detection, keyboard nav, rendering.
    /// `allow`: false while wire-cutting is active.
    pub(crate) fn update(
        &mut self,
        ui: &mut egui::Ui,
        response: &egui::Response,
        _pointer: Option<Pos2>,
        _allow: bool,
    ) -> Option<GraphAction> {
        if let Some(action) = self.popup.handle_keys(ui, response) {
            return self.handle_popup_action(action);
        }
        if let Some(action) = self.popup.render(ui, response) {
            return self.handle_popup_action(action);
        }
        self.add_search.render(ui)
    }

    fn handle_popup_action(&mut self, action: GraphAction) -> Option<GraphAction> {
        match action {
            GraphAction::OpenAddSearch => {
                self.add_search
                    .open(self.add_popup_pos, self.add_search_items.clone());
                None
            }
            action => Some(action),
        }
    }
}
