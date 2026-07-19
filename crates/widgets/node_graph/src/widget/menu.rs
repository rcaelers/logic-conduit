use egui::{Color32, Pos2};
pub(crate) use input_bindings::MenuShortcut as Shortcut;
use widget_support::{MENU_ICON_COLUMN_WIDTH, menu_item_layout_job};

// ── Shortcut ──────────────────────────────────────────────────────────────────

// ── MenuEntry ─────────────────────────────────────────────────────────────────

pub(crate) enum MenuKind<T> {
    Separator,
    Action(T),
    SubMenu(Vec<MenuEntry<T>>),
    Palette(Vec<(Color32, T)>),
}

pub(crate) struct MenuEntry<T> {
    pub label: String,
    pub icon: Option<String>,
    pub checked: Option<bool>,
    pub shortcut: Option<Shortcut>,
    pub kind: MenuKind<T>,
}

impl<T> MenuEntry<T> {
    pub(super) fn action(label: impl Into<String>, action: T) -> Self {
        Self {
            label: label.into(),
            icon: None,
            checked: None,
            shortcut: None,
            kind: MenuKind::Action(action),
        }
    }

    /// Creates a submenu entry.  The `▶` arrow is added automatically by the renderer.
    pub(super) fn submenu(label: impl Into<String>, children: Vec<MenuEntry<T>>) -> Self {
        Self {
            label: label.into(),
            icon: None,
            checked: None,
            shortcut: None,
            kind: MenuKind::SubMenu(children),
        }
    }

    pub(super) fn palette(items: Vec<(Color32, T)>) -> Self {
        Self {
            label: String::new(),
            icon: None,
            checked: None,
            shortcut: None,
            kind: MenuKind::Palette(items),
        }
    }

    pub(super) fn separator() -> Self {
        Self {
            label: String::new(),
            icon: None,
            checked: None,
            shortcut: None,
            kind: MenuKind::Separator,
        }
    }

    pub(super) fn with_shortcut(mut self, sc: Shortcut) -> Self {
        self.shortcut = Some(sc);
        self
    }

    pub(super) fn with_icon(mut self, icon: impl Into<String>) -> Self {
        self.icon = Some(icon.into());
        self
    }

    /// Reserves the menu icon column for a toggle state. A checked entry uses
    /// a painted tick rather than a font glyph, avoiding platform fallback
    /// boxes for checkmark characters.
    pub(super) fn with_checkmark(mut self, checked: bool) -> Self {
        self.checked = Some(checked);
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

pub(crate) fn dispatch_menu_shortcut<T: Clone>(
    ui: &mut egui::Ui,
    entries: &[MenuEntry<T>],
) -> Option<T> {
    for entry in entries {
        match &entry.kind {
            MenuKind::Action(action) => {
                if let Some(shortcut) = entry.shortcut
                    && shortcut.consume(ui)
                {
                    return Some(action.clone());
                }
            }
            MenuKind::SubMenu(children) => {
                if let Some(action) = dispatch_menu_shortcut(ui, children) {
                    return Some(action);
                }
            }
            MenuKind::Palette(_) => {}
            MenuKind::Separator => {}
        }
    }
    None
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
    title: Option<String>,
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
    const COLUMN_OVERLAP: f32 = 10.0;

    pub(super) fn new(area_id: egui::Id) -> Self {
        Self {
            area_id,
            entries: Vec::new(),
            title: None,
            sel: Vec::new(),
            btn_ids: Vec::new(),
            visible: false,
            pos: Pos2::ZERO,
            pending_close: false,
        }
    }

    pub(super) fn set_entries(&mut self, entries: Vec<MenuEntry<T>>) {
        self.entries = entries;
        self.title = None;
    }

    pub(super) fn set_entries_with_title(
        &mut self,
        title: impl Into<String>,
        entries: Vec<MenuEntry<T>>,
    ) {
        self.entries = entries;
        self.title = Some(title.into());
    }

    pub(super) fn open(&mut self, pos: Pos2) {
        self.visible = true;
        self.pos = pos;
        self.sel.clear();
        self.btn_ids.clear();
        self.pending_close = false;
    }

    pub(super) fn close(&mut self) {
        self.visible = false;
        self.sel.clear();
        self.pending_close = false;
    }

    fn close_and_surrender_focus(&mut self, ui: &mut egui::Ui) {
        ui.memory_mut(|memory| memory.surrender_focus(self.area_id));
        self.close();
    }

    // ── Keyboard navigation ───────────────────────────────────────────────────

    /// `root_id` — context menus: `egui::Popup::default_response_id(response)`;
    ///             standalone popups: `self.area_id`.
    pub(super) fn handle_keys(&mut self, ui: &mut egui::Ui, root_id: egui::Id) -> Option<T> {
        // Claim keyboard focus while the menu is handling input.  This makes
        // `memory().focused()` return Some so the host widget's `no_focus`
        // guard naturally blocks global shortcuts like Shift+A.
        ui.memory_mut(|m| m.request_focus(self.area_id));

        if self.entries.is_empty() {
            return None;
        }

        if let Some(action) = dispatch_menu_shortcut(ui, &self.entries) {
            self.sel.clear();
            self.pending_close = true;
            return Some(action);
        }

        let down = ui.input(|input| input.key_pressed(egui::Key::ArrowDown));
        let up = ui.input(|input| input.key_pressed(egui::Key::ArrowUp));
        let right = ui.input(|input| input.key_pressed(egui::Key::ArrowRight));
        let left = ui.input(|input| input.key_pressed(egui::Key::ArrowLeft));
        let enter = ui.input(|input| input.key_pressed(egui::Key::Enter));

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
                    self.close_and_surrender_focus(ui);
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
                    self.close_and_surrender_focus(ui);
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
    pub(super) fn show_popup(&mut self, ui: &mut egui::Ui) -> (egui::Response, Option<T>) {
        let entries = std::mem::take(&mut self.entries);
        let title = self.title.clone();
        let mut sel = self.sel.clone();
        let area_id = self.area_id;
        let pos = self.pos;
        let menu_signature = Self::entries_signature(&entries);

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
                ui.spacing_mut().item_spacing.x = -Self::COLUMN_OVERLAP;
                ui.horizontal_top(|ui| {
                    let mut depth = 0;
                    let mut column_offsets = vec![0.0_f32];
                    loop {
                        let column_path = Self::entry_path(&entries, &sel[..depth]);
                        let depth_entries = Self::entries_at(&entries, &sel[..depth]);
                        if depth_entries.is_empty() {
                            break;
                        }

                        let column_offset = column_offsets.get(depth).copied().unwrap_or(0.0);
                        let mut next_column_offset = None;
                        ui.push_id(("menu-column", &menu_signature, &column_path), |ui| {
                            ui.vertical(|ui| {
                                ui.add_space(column_offset);
                                egui::Frame::menu(ui.style()).show(ui, |ui| {
                                    ui.allocate_ui_with_layout(
                                        egui::vec2(Self::MIN_WIDTH, 0.0),
                                        egui::Layout::top_down_justified(egui::Align::Min),
                                        |ui| {
                                            next_column_offset = Self::render_column(
                                                ui,
                                                depth_entries,
                                                &mut sel,
                                                &column_path,
                                                depth,
                                                (depth == 0).then_some(title.as_deref()).flatten(),
                                                &mut result,
                                            )
                                            .map(|offset| column_offset + offset);
                                        },
                                    );
                                });
                            });
                        });

                        if result.is_some() {
                            break;
                        }
                        let Some(&selected) = sel.get(depth) else {
                            break;
                        };
                        if !depth_entries
                            .get(selected)
                            .is_some_and(MenuEntry::is_submenu)
                        {
                            break;
                        }

                        if column_offsets.len() <= depth + 1 {
                            column_offsets.push(next_column_offset.unwrap_or(column_offset));
                        } else if let Some(offset) = next_column_offset {
                            column_offsets[depth + 1] = offset;
                        }
                        depth += 1;
                    }
                });
            });

        self.entries = entries;
        self.sel = sel;
        self.btn_ids.clear();
        (area_resp.response, result)
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

    /// Semantic ancestry of the column currently being rendered. A submenu
    /// column can occupy the same screen rect as a different branch on the
    /// next frame, so its UI scope must follow the branch rather than depth.
    fn entry_path(root: &[MenuEntry<T>], path: &[usize]) -> Vec<String> {
        let mut entries = root;
        let mut labels = Vec::with_capacity(path.len());
        for &index in path {
            let Some(entry) = entries.get(index) else {
                break;
            };
            labels.push(entry.label.clone());
            if !entry.is_submenu() {
                break;
            }
            entries = entry.children();
        }
        labels
    }

    /// Identifies a menu tree independently of transient hover selection.
    /// A context menu can replace its entries while remaining at the same
    /// screen position, so its root scope must change with its contents too.
    fn entries_signature(entries: &[MenuEntry<T>]) -> String {
        fn append<T>(signature: &mut String, entries: &[MenuEntry<T>]) {
            for entry in entries {
                match &entry.kind {
                    MenuKind::Separator => signature.push_str("separator"),
                    MenuKind::Action(_) => signature.push_str("action"),
                    MenuKind::Palette(_) => signature.push_str("palette"),
                    MenuKind::SubMenu(children) => {
                        signature.push_str("submenu[");
                        append(signature, children);
                        signature.push(']');
                    }
                }
                signature.push(':');
                signature.push_str(&entry.label);
                signature.push('|');
            }
        }

        let mut signature = String::new();
        append(&mut signature, entries);
        signature
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

    fn render_column(
        ui: &mut egui::Ui,
        entries: &[MenuEntry<T>],
        sel: &mut Vec<usize>,
        column_path: &[String],
        depth: usize,
        title: Option<&str>,
        result: &mut Option<T>,
    ) -> Option<f32> {
        ui.set_min_width(Self::MIN_WIDTH);

        let sel_bg = ui.visuals().selection.bg_fill;
        let sel_at_depth = sel.get(depth).copied();
        if let Some(title) = title {
            ui.add_space(2.0);
            ui.label(egui::RichText::new(title).strong());
            ui.separator();
        }
        let column_top = ui.next_widget_position().y;
        let mut selected_submenu_offset = None;

        for (i, entry) in entries.iter().enumerate() {
            if entry.is_separator() {
                ui.separator();
                continue;
            }

            let highlighted = sel_at_depth == Some(i) && result.is_none();
            let fill = if highlighted {
                sel_bg
            } else {
                Color32::TRANSPARENT
            };
            ui.push_id(("menu-entry", column_path, &entry.label, i), |ui| {
                let job = menu_item_layout_job(ui, entry.icon.as_deref(), &entry.label);

                match &entry.kind {
                    MenuKind::SubMenu(_) => {
                        let arrow = egui::containers::menu::SubMenuButton::RIGHT_ARROW;
                        let btn = egui::Button::new(egui::WidgetText::LayoutJob(job.into()))
                            .right_text(arrow)
                            .fill(fill)
                            .wrap_mode(egui::TextWrapMode::Extend);
                        let resp = ui.add(btn);
                        if resp.hovered() || resp.clicked() {
                            if sel.len() <= depth {
                                sel.resize(depth + 1, 0);
                            }
                            sel[depth] = i;
                            sel.truncate(depth + 1);
                        }
                        if sel.get(depth) == Some(&i) {
                            selected_submenu_offset = Some(resp.rect.min.y - column_top);
                        }
                    }
                    MenuKind::Action(action) => {
                        let mut btn = egui::Button::new(egui::WidgetText::LayoutJob(job.into()))
                            .fill(fill)
                            .wrap_mode(egui::TextWrapMode::Extend);
                        if let Some(sc) = entry.shortcut {
                            btn = btn.right_text(sc.to_string());
                        }
                        let resp = ui.add(btn);
                        if entry.checked == Some(true) {
                            draw_checkmark(ui, &resp);
                        }
                        if resp.hovered() {
                            if sel.len() <= depth {
                                sel.resize(depth + 1, 0);
                            }
                            sel[depth] = i;
                            sel.truncate(depth + 1);
                        }
                        if resp.clicked() {
                            *result = Some(action.clone());
                            ui.close();
                        }
                    }
                    MenuKind::Palette(items) => {
                        Self::render_palette(ui, items, result);
                    }
                    MenuKind::Separator => unreachable!(),
                }
            });
        }
        selected_submenu_offset
    }

    fn render_palette(ui: &mut egui::Ui, items: &[(Color32, T)], result: &mut Option<T>) {
        const COLUMNS: usize = 8;
        const SWATCH: f32 = 18.0;
        const GAP: f32 = 3.0;

        ui.spacing_mut().item_spacing = egui::vec2(GAP, GAP);
        for row in items.chunks(COLUMNS) {
            ui.horizontal(|ui| {
                for &(color, ref action) in row {
                    let (rect, response) =
                        ui.allocate_exact_size(egui::vec2(SWATCH, SWATCH), egui::Sense::click());
                    let rounding = egui::CornerRadius::same(3);
                    ui.painter().rect_filled(rect, rounding, color);
                    let stroke = if response.hovered() {
                        egui::Stroke::new(2.0, Color32::WHITE)
                    } else {
                        egui::Stroke::new(1.0, Color32::from_rgb(75, 75, 75))
                    };
                    ui.painter()
                        .rect_stroke(rect, rounding, stroke, egui::StrokeKind::Outside);
                    if response.clicked() {
                        *result = Some(action.clone());
                        ui.close();
                    }
                }
            });
        }
    }
}

fn draw_checkmark(ui: &egui::Ui, response: &egui::Response) {
    let center = egui::pos2(
        response.rect.left() + MENU_ICON_COLUMN_WIDTH * 0.5,
        response.rect.center().y,
    );
    let stroke = egui::Stroke::new(2.0, ui.visuals().widgets.style(response).fg_stroke.color);
    ui.painter().line_segment(
        [
            center + egui::vec2(-5.0, 0.0),
            center + egui::vec2(-1.5, 3.5),
        ],
        stroke,
    );
    ui.painter().line_segment(
        [
            center + egui::vec2(-1.5, 3.5),
            center + egui::vec2(5.5, -4.0),
        ],
        stroke,
    );
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
}

impl<T: Clone> PopupMenu<T> {
    pub(super) fn new(popup_id: egui::Id) -> Self {
        Self {
            popup: Menu::new(popup_id),
        }
    }

    /// Open the standalone popup at screen position `pos` with the given entries.
    pub(super) fn open_popup(&mut self, pos: Pos2, entries: Vec<MenuEntry<T>>) {
        self.popup.set_entries(entries);
        self.popup.open(pos);
    }

    pub(super) fn open_popup_with_title(
        &mut self,
        pos: Pos2,
        title: impl Into<String>,
        entries: Vec<MenuEntry<T>>,
    ) {
        self.popup.set_entries_with_title(title, entries);
        self.popup.open(pos);
    }

    pub(super) fn is_open(&self) -> bool {
        self.popup.visible
    }

    /// Drive keyboard navigation for whichever menu is currently active and
    /// claim keyboard focus so the host's `no_focus` check naturally suppresses
    /// global shortcuts while any menu is open.
    ///
    /// **Must be called before sampling `no_focus`.**
    pub(super) fn handle_keys(
        &mut self,
        ui: &mut egui::Ui,
        _response: &egui::Response,
    ) -> Option<T> {
        if self.popup.visible {
            let id = self.popup.area_id;
            let result = self.popup.handle_keys(ui, id);
            if result.is_some() {
                self.popup.close_and_surrender_focus(ui);
            }
            result
        } else {
            None
        }
    }

    /// Render whichever menu is active and return any mouse-activated action.
    pub(super) fn render(&mut self, ui: &mut egui::Ui, _response: &egui::Response) -> Option<T> {
        let mut result = None;

        // Standalone popup — we own its lifecycle entirely.
        if self.popup.visible {
            let escape = ui.input(|input| input.key_pressed(egui::Key::Escape));
            let sec_press =
                ui.input(|input| input.pointer.button_pressed(egui::PointerButton::Secondary));
            if escape || sec_press {
                self.popup.close_and_surrender_focus(ui);
            } else {
                let (area_resp, clicked) = self.popup.show_popup(ui);
                if clicked.is_some() {
                    result = clicked;
                    self.popup.close_and_surrender_focus(ui);
                } else if !area_resp.hovered()
                    && ui.input(|input| input.pointer.button_released(egui::PointerButton::Primary))
                {
                    self.popup.close_and_surrender_focus(ui);
                }
            }
        }

        result
    }
}

#[cfg(test)]
mod tests {
    use super::{Menu, MenuEntry};

    fn entries() -> Vec<MenuEntry<()>> {
        vec![MenuEntry::submenu(
            "Add",
            vec![MenuEntry::submenu(
                "Logic",
                vec![MenuEntry::action("Buffer", ())],
            )],
        )]
    }

    #[test]
    fn closing_popup_surrenders_its_keyboard_focus() {
        let ctx = egui::Context::default();
        let _ = ctx.run_ui(Default::default(), |ui| {
            let id = egui::Id::new("focused-popup");
            let mut menu = Menu::<()>::new(id);
            menu.open(egui::Pos2::ZERO);
            ui.memory_mut(|memory| memory.request_focus(id));
            assert_eq!(ui.memory(|memory| memory.focused()), Some(id));

            menu.close_and_surrender_focus(ui);

            assert!(!menu.visible);
            assert_eq!(ui.memory(|memory| memory.focused()), None);
        });
    }

    #[test]
    fn menu_column_path_uses_semantic_ancestry() {
        let entries = entries();

        assert_eq!(
            Menu::<()>::entry_path(&entries, &[0, 0]),
            vec!["Add".to_owned(), "Logic".to_owned()]
        );
    }

    #[test]
    fn menu_signature_changes_when_the_tree_changes() {
        let first = entries();
        let second = vec![MenuEntry::action("Delete", ())];

        assert_ne!(
            Menu::<()>::entries_signature(&first),
            Menu::<()>::entries_signature(&second)
        );
    }
}
