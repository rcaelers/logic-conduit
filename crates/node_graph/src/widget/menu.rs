use egui::{Color32, Pos2};
use std::fmt;

// ── Shortcut ──────────────────────────────────────────────────────────────────

#[derive(Clone, Copy)]
pub(crate) struct Shortcut {
    pub modifiers: egui::Modifiers,
    pub key: egui::Key,
}

impl Shortcut {
    pub fn key(key: egui::Key) -> Self {
        Self {
            modifiers: egui::Modifiers::NONE,
            key,
        }
    }
    pub fn ctrl(key: egui::Key) -> Self {
        Self {
            modifiers: egui::Modifiers {
                ctrl: true,
                ..egui::Modifiers::NONE
            },
            key,
        }
    }
    pub fn command(key: egui::Key) -> Self {
        Self {
            modifiers: egui::Modifiers::COMMAND,
            key,
        }
    }
    pub fn shift(key: egui::Key) -> Self {
        Self {
            modifiers: egui::Modifiers {
                shift: true,
                ..egui::Modifiers::NONE
            },
            key,
        }
    }

    pub fn matches(&self, key: egui::Key, modifiers: egui::Modifiers) -> bool {
        if key != self.key {
            return false;
        }
        if self.modifiers.command {
            return modifiers.command
                && modifiers.shift == self.modifiers.shift
                && modifiers.alt == self.modifiers.alt;
        }
        modifiers == self.modifiers
    }
}

impl fmt::Display for Shortcut {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.modifiers.command {
            if cfg!(target_os = "macos") {
                write!(f, "⌘ ")?;
            } else {
                write!(f, "^ ")?;
            }
        }
        if self.modifiers.ctrl {
            write!(f, "^")?;
        }
        if self.modifiers.shift {
            write!(f, "Shift+")?;
        }
        if self.modifiers.alt {
            write!(f, "Alt+")?;
        }
        write!(f, "{}", self.key.name())
    }
}

// ── MenuEntry ─────────────────────────────────────────────────────────────────

pub(crate) enum MenuKind<T> {
    Separator,
    Action(T),
    SubMenu(Vec<MenuEntry<T>>),
}

pub(crate) struct MenuEntry<T> {
    pub label: String,
    pub icon: Option<String>,
    pub shortcut: Option<Shortcut>,
    pub kind: MenuKind<T>,
}

impl<T> MenuEntry<T> {
    pub fn action(label: impl Into<String>, action: T) -> Self {
        Self {
            label: label.into(),
            icon: None,
            shortcut: None,
            kind: MenuKind::Action(action),
        }
    }

    /// Creates a submenu entry.  The `▶` arrow is added automatically by the renderer.
    pub fn submenu(label: impl Into<String>, children: Vec<MenuEntry<T>>) -> Self {
        Self {
            label: label.into(),
            icon: None,
            shortcut: None,
            kind: MenuKind::SubMenu(children),
        }
    }

    pub fn separator() -> Self {
        Self {
            label: String::new(),
            icon: None,
            shortcut: None,
            kind: MenuKind::Separator,
        }
    }

    pub fn with_shortcut(mut self, sc: Shortcut) -> Self {
        self.shortcut = Some(sc);
        self
    }

    pub fn with_icon(mut self, icon: impl Into<String>) -> Self {
        self.icon = Some(icon.into());
        self
    }

    fn is_separator(&self) -> bool {
        matches!(self.kind, MenuKind::Separator)
    }

    fn is_submenu(&self) -> bool {
        matches!(self.kind, MenuKind::SubMenu(_))
    }

    fn children(&self) -> &[MenuEntry<T>] {
        match &self.kind {
            MenuKind::SubMenu(c) => c,
            _ => &[],
        }
    }
}

// ── Menu widget ───────────────────────────────────────────────────────────────

/// Stateful multi-level menu widget.  `T` is the caller-defined action type.
///
/// Usage pattern:
/// 1. [`Menu::set_entries`] to supply the current item tree.
/// 2. [`Menu::handle_keys`] every frame while the menu is open.
/// 3. [`Menu::show_popup`] or [`Menu::show_in_context`] to render.
/// 4. Combine the `Option<T>` returns — the caller acts on the activated value.
pub(crate) struct Menu<T> {
    area_id: egui::Id,
    entries: Vec<MenuEntry<T>>,
    /// Keyboard nav path: `sel[d]` = selected item index at depth `d`.
    sel: Vec<usize>,
    /// SubMenuButton IDs captured last frame: `btn_ids[d][i]`.
    btn_ids: Vec<Vec<egui::Id>>,
    pub visible: bool,
    pub pos: Pos2,
    pending_close: bool,
}

impl<T: Clone> Menu<T> {
    const MIN_WIDTH: f32 = 180.0;

    pub fn new(area_id: egui::Id) -> Self {
        Self {
            area_id,
            entries: Vec::new(),
            sel: Vec::new(),
            btn_ids: Vec::new(),
            visible: false,
            pos: Pos2::ZERO,
            pending_close: false,
        }
    }

    pub fn set_entries(&mut self, entries: Vec<MenuEntry<T>>) {
        self.entries = entries;
    }

    pub fn open(&mut self, pos: Pos2) {
        self.visible = true;
        self.pos = pos;
        self.sel.clear();
        self.btn_ids.clear();
        self.pending_close = false;
    }

    pub fn close(&mut self) {
        self.visible = false;
        self.sel.clear();
        self.pending_close = false;
    }

    pub fn reset(&mut self) {
        self.sel.clear();
        self.pending_close = false;
    }

    // ── Keyboard navigation ───────────────────────────────────────────────────

    /// `root_id` — context menus: `egui::Popup::default_response_id(response)`;
    ///             standalone popups: `self.area_id`.
    pub fn handle_keys(&mut self, ui: &mut egui::Ui, root_id: egui::Id) -> Option<T> {
        // Claim keyboard focus while the menu is handling input.  This makes
        // `memory().focused()` return Some so the host widget's `no_focus`
        // guard naturally blocks global shortcuts like Shift+A.
        ui.memory_mut(|m| m.request_focus(self.area_id));

        if self.entries.is_empty() {
            return None;
        }

        let down = ui.input(|i| i.key_pressed(egui::Key::ArrowDown));
        let up = ui.input(|i| i.key_pressed(egui::Key::ArrowUp));
        let right = ui.input(|i| i.key_pressed(egui::Key::ArrowRight));
        let left = ui.input(|i| i.key_pressed(egui::Key::ArrowLeft));
        let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));

        if !down && !up && !right && !left && !enter {
            if !self.sel.is_empty() {
                self.drive_menu_state(ui, root_id);
            }
            return None;
        }

        if self.sel.is_empty() {
            let n = self.entries.len();
            if n == 0 {
                return None;
            }
            if left && !down && !up && !right && !enter {
                if self.visible {
                    self.close();
                }
                egui::containers::menu::MenuState::from_id(ui.ctx(), root_id, |s| {
                    s.open_item = None
                });
                return None;
            }
            let start = if up { n.saturating_sub(1) } else { 0 };
            self.sel.push(Self::skip_sep(&self.entries, start, !up));
        }

        let depth = self.sel.len() - 1;

        if down {
            let cur = Self::entries_at(&self.entries, &self.sel[..depth]);
            let n = cur.len();
            let t = (self.sel[depth] + 1).min(n.saturating_sub(1));
            self.sel[depth] = Self::skip_sep(cur, t, true);
        }
        if up {
            let cur = Self::entries_at(&self.entries, &self.sel[..depth]);
            let t = self.sel[depth].saturating_sub(1);
            self.sel[depth] = Self::skip_sep(cur, t, false);
        }

        let depth = self.sel.len() - 1;
        let current_is_submenu = {
            let cur = Self::entries_at(&self.entries, &self.sel[..depth]);
            cur.get(self.sel[depth]).is_some_and(|e| e.is_submenu())
        };
        let current_action: Option<T> = {
            let cur = Self::entries_at(&self.entries, &self.sel[..depth]);
            cur.get(self.sel[depth]).and_then(|e| match &e.kind {
                MenuKind::Action(t) => Some(t.clone()),
                _ => None,
            })
        };

        if (right || (enter && current_is_submenu)) && current_is_submenu {
            let sub_len = {
                let cur = Self::entries_at(&self.entries, &self.sel[..depth]);
                cur.get(self.sel[depth]).map_or(0, |e| e.children().len())
            };
            if sub_len > 0 {
                // Compute starting index in the submenu before pushing to sel.
                let sub_start = {
                    let cur = Self::entries_at(&self.entries, &self.sel[..depth]);
                    let ch = cur.get(self.sel[depth]).map_or(&[][..], |e| e.children());
                    Self::skip_sep(ch, 0, true)
                };
                self.sel.push(sub_start);
            }
        }

        if left {
            if self.sel.len() > 1 {
                self.sel.pop();
            } else {
                self.sel.clear();
                egui::containers::menu::MenuState::from_id(ui.ctx(), root_id, |s| {
                    s.open_item = None
                });
                if self.visible {
                    self.close();
                }
                return None;
            }
        }

        let mut result = None;
        if enter
            && !current_is_submenu
            && let Some(action) = current_action
        {
            self.sel.clear();
            self.pending_close = true;
            result = Some(action);
        }

        self.drive_menu_state(ui, root_id);
        result
    }

    // ── Rendering ─────────────────────────────────────────────────────────────

    /// Standalone `Area` popup.  Returns `(area_response, activated_action)`.
    pub fn show_popup(&mut self, ui: &mut egui::Ui) -> (egui::Response, Option<T>) {
        let entries = std::mem::take(&mut self.entries);
        let sel = self.sel.clone();
        let area_id = self.area_id;
        let pos = self.pos;

        let mut new_btn_ids: Vec<Vec<egui::Id>> = Vec::new();
        let mut result: Option<T> = None;

        let area_resp = egui::Area::new(area_id)
            .fixed_pos(pos)
            .order(egui::Order::Foreground)
            .layout(egui::Layout::top_down_justified(egui::Align::Min))
            .info(egui::UiStackInfo::new(egui::UiKind::Menu).with_tag_value(
                egui::containers::menu::MenuConfig::MENU_CONFIG_TAG,
                egui::containers::menu::MenuConfig::new(),
            ))
            .show(ui.ctx(), |ui| {
                egui::containers::menu::menu_style(ui.style_mut());
                egui::Frame::menu(ui.style()).show(ui, |ui| {
                    Self::render_entries(ui, &entries, &sel, 0, &mut new_btn_ids, &mut result);
                });
            });

        self.entries = entries;
        self.btn_ids = new_btn_ids;
        (area_resp.response, result)
    }

    /// Render inside `response.context_menu()`.  Returns activated action on click.
    pub fn show_in_context(&mut self, ui: &mut egui::Ui) -> Option<T> {
        if self.pending_close {
            ui.close();
            self.pending_close = false;
            return None;
        }

        let entries = std::mem::take(&mut self.entries);
        let sel = self.sel.clone();
        let mut new_btn_ids: Vec<Vec<egui::Id>> = Vec::new();
        let mut result: Option<T> = None;

        Self::render_entries(ui, &entries, &sel, 0, &mut new_btn_ids, &mut result);

        self.entries = entries;
        self.btn_ids = new_btn_ids;
        result
    }

    // ── Internals ─────────────────────────────────────────────────────────────

    fn entries_at<'a>(root: &'a [MenuEntry<T>], path: &[usize]) -> &'a [MenuEntry<T>] {
        let mut cur = root;
        for &idx in path {
            match cur.get(idx) {
                Some(e) if e.is_submenu() => cur = e.children(),
                _ => return &[],
            }
        }
        cur
    }

    fn skip_sep(entries: &[MenuEntry<T>], mut i: usize, forward: bool) -> usize {
        let len = entries.len();
        if len == 0 {
            return 0;
        }
        loop {
            if i >= len || !entries[i].is_separator() {
                break;
            }
            if forward {
                if i + 1 >= len {
                    break;
                }
                i += 1;
            } else {
                if i == 0 {
                    break;
                }
                i -= 1;
            }
        }
        i
    }

    fn drive_menu_state(&self, ui: &mut egui::Ui, root_id: egui::Id) {
        if self.sel.is_empty() {
            return;
        }
        egui::containers::menu::MenuState::mark_shown(ui.ctx(), root_id);

        let mut menu_root = root_id;
        for d in 0..self.sel.len() {
            let sel_i = self.sel[d];
            let depth_entries = Self::entries_at(&self.entries, &self.sel[..d]);
            match depth_entries.get(sel_i) {
                Some(entry) if entry.is_submenu() => {
                    if let Some(btn_id) =
                        self.btn_ids.get(d).and_then(|ids| ids.get(sel_i)).copied()
                    {
                        if btn_id == egui::Id::NULL {
                            break;
                        }
                        let sub_id = egui::containers::menu::SubMenu::id_from_widget_id(btn_id);
                        egui::containers::menu::MenuState::mark_shown(ui.ctx(), sub_id);
                        egui::containers::menu::MenuState::from_id(ui.ctx(), menu_root, |s| {
                            s.open_item = Some(sub_id)
                        });
                        menu_root = sub_id;
                    }
                }
                Some(_) => {
                    egui::containers::menu::MenuState::from_id(ui.ctx(), menu_root, |s| {
                        s.open_item = None
                    });
                }
                None => {}
            }
        }
    }

    /// Width of the icon column.  All label text starts at this x-offset so
    /// items with and without icons stay aligned.
    const ICON_COL_W: f32 = 22.0;

    /// Build a [`LayoutJob`] whose icon section occupies exactly [`ICON_COL_W`]
    /// points, with the label starting at that fixed offset.  Uses
    /// `Color32::PLACEHOLDER` so egui's button widget applies the correct
    /// hover/active text colour automatically.
    fn item_layout_job(ui: &egui::Ui, icon: Option<&str>, label: &str) -> egui::text::LayoutJob {
        let font_id = egui::TextStyle::Button.resolve(ui.style());
        let ph = egui::Color32::PLACEHOLDER;
        let fmt = egui::TextFormat {
            font_id: font_id.clone(),
            color: ph,
            ..Default::default()
        };

        let mut job = egui::text::LayoutJob::default();
        if let Some(s) = icon {
            let icon_w = ui.ctx().fonts_mut(|f| {
                f.layout_no_wrap(s.to_string(), font_id.clone(), ph)
                    .size()
                    .x
            });
            job.append(
                s,
                0.0,
                egui::TextFormat {
                    font_id: font_id.clone(),
                    color: ph,
                    ..Default::default()
                },
            );
            job.append(label, (Self::ICON_COL_W - icon_w).max(2.0), fmt);
        } else {
            job.append(label, Self::ICON_COL_W, fmt);
        }
        job
    }

    fn render_entries(
        ui: &mut egui::Ui,
        entries: &[MenuEntry<T>],
        sel: &[usize],
        depth: usize,
        btn_ids: &mut Vec<Vec<egui::Id>>,
        result: &mut Option<T>,
    ) {
        ui.set_min_width(Self::MIN_WIDTH);

        let sel_bg = ui.visuals().selection.bg_fill;
        let sel_at_depth = sel.get(depth).copied();

        if btn_ids.len() <= depth {
            btn_ids.resize(depth + 1, Vec::new());
        }
        btn_ids[depth].clear();

        for (i, entry) in entries.iter().enumerate() {
            if entry.is_separator() {
                ui.separator();
                btn_ids[depth].push(egui::Id::NULL);
                continue;
            }

            let highlighted = sel_at_depth == Some(i) && result.is_none();
            let fill = if highlighted {
                sel_bg
            } else {
                Color32::TRANSPARENT
            };
            let job = Self::item_layout_job(ui, entry.icon.as_deref(), &entry.label);

            match &entry.kind {
                MenuKind::SubMenu(children) => {
                    let arrow = egui::containers::menu::SubMenuButton::RIGHT_ARROW;
                    let btn = egui::Button::new(egui::WidgetText::LayoutJob(job.into()))
                        .right_text(arrow)
                        .fill(fill);
                    let (resp, _) =
                        egui::containers::menu::SubMenuButton::from_button(btn).ui(ui, |ui| {
                            Self::render_entries(ui, children, sel, depth + 1, btn_ids, result);
                        });
                    btn_ids[depth].push(resp.id);
                }
                MenuKind::Action(action) => {
                    let mut btn =
                        egui::Button::new(egui::WidgetText::LayoutJob(job.into())).fill(fill);
                    if let Some(sc) = entry.shortcut {
                        btn = btn.right_text(sc.to_string());
                    }
                    if ui.add(btn).clicked() {
                        *result = Some(action.clone());
                        ui.close();
                    }
                    btn_ids[depth].push(egui::Id::NULL);
                }
                MenuKind::Separator => unreachable!(),
            }
        }
    }
}

// ── PopupMenu ─────────────────────────────────────────────────────────────────

/// Unified menu controller owning both a standalone popup (e.g. Shift+A) and
/// a right-click context menu as a single host-side field.
///
/// Call [`handle_keys`] then [`render`] each frame.  All visibility checks and
/// lifecycle management are handled internally; the host never inspects
/// `visible` directly.
pub(crate) struct PopupMenu<T> {
    popup: Menu<T>,
    context: Menu<T>,
}

impl<T: Clone> PopupMenu<T> {
    pub fn new(popup_id: egui::Id) -> Self {
        Self {
            popup: Menu::new(popup_id),
            context: Menu::new(egui::Id::new("__popup_ctx__")),
        }
    }

    /// Open the standalone popup at screen position `pos` with the given entries.
    pub fn open_popup(&mut self, pos: Pos2, entries: Vec<MenuEntry<T>>) {
        self.popup.set_entries(entries);
        self.popup.open(pos);
    }

    /// Supply entries for the right-click context menu.  Called each frame
    /// while the context menu may be open; ignored when it is not.
    pub fn set_context_entries(&mut self, entries: Vec<MenuEntry<T>>) {
        self.context.set_entries(entries);
    }

    /// Drive keyboard navigation for whichever menu is currently active and
    /// claim keyboard focus so the host's `no_focus` check naturally suppresses
    /// global shortcuts while any menu is open.
    ///
    /// **Must be called before sampling `no_focus`.**
    pub fn handle_keys(&mut self, ui: &mut egui::Ui, response: &egui::Response) -> Option<T> {
        if self.popup.visible {
            let id = self.popup.area_id;
            let result = self.popup.handle_keys(ui, id);
            if result.is_some() {
                self.popup.close();
            }
            result
        } else if response.context_menu_opened() {
            let id = egui::Popup::default_response_id(response);
            self.context.handle_keys(ui, id)
        } else {
            self.context.reset();
            None
        }
    }

    /// Render whichever menu is active and return any mouse-activated action.
    pub fn render(&mut self, ui: &mut egui::Ui, response: &egui::Response) -> Option<T> {
        let mut result = None;

        // Right-click context menu — egui owns its lifecycle (Escape, outside click).
        response.context_menu(|ui| {
            result = self.context.show_in_context(ui);
        });

        // Standalone popup — we own its lifecycle entirely.
        if self.popup.visible {
            let escape = ui.input(|i| i.key_pressed(egui::Key::Escape));
            let sec_press = ui.input(|i| i.pointer.button_pressed(egui::PointerButton::Secondary));
            if escape || sec_press {
                self.popup.close();
            } else {
                let (area_resp, clicked) = self.popup.show_popup(ui);
                if clicked.is_some() {
                    result = clicked;
                    self.popup.close();
                } else if !area_resp.hovered()
                    && ui.input(|i| i.pointer.button_released(egui::PointerButton::Primary))
                {
                    self.popup.close();
                }
            }
        }

        result
    }
}
