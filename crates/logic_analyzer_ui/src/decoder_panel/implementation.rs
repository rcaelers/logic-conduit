use std::collections::{HashMap, HashSet};

use egui::{Color32, PopupCloseBehavior, Stroke};

use logic_analyzer_graph::{
    DecoderTableCellMode, DecoderTableColumn, DecoderTableRegistry, DecoderTableSource,
};
use logic_analyzer_viewer::AnnotationVisual;
use signal_processing::{Annotation, DerivedLaneData, DerivedLanes};

const MAX_TABLE_ROWS: usize = 100_000;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
enum DataFormat {
    #[default]
    Lane,
    Hex,
    Ascii,
    HexAscii,
}

impl DataFormat {
    fn label(self) -> &'static str {
        match self {
            Self::Lane => "Lane format",
            Self::Hex => "Hex",
            Self::Ascii => "ASCII",
            Self::HexAscii => "Hex + ASCII",
        }
    }

    fn override_name(self) -> Option<&'static str> {
        match self {
            Self::Lane => None,
            Self::Hex => Some("Hex"),
            Self::Ascii => Some("ASCII"),
            Self::HexAscii => Some("Hex + ASCII"),
        }
    }
}

const FORMAT_CHOICES: [DataFormat; 3] = [DataFormat::Hex, DataFormat::Ascii, DataFormat::HexAscii];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ToolbarMenu {
    Decoder,
    Format,
    Columns,
}

#[derive(Clone, Default, serde::Deserialize, serde::Serialize)]
struct DecoderPanelState {
    selected_source: Option<String>,
    hidden_columns: HashSet<String>,
    format: DataFormat,
    #[serde(skip)]
    popup_focus: usize,
    #[serde(skip)]
    open_menu: Option<ToolbarMenu>,
}

#[derive(Clone, Default, serde::Deserialize, serde::Serialize)]
pub(crate) struct DecoderPanelsState {
    panels: HashMap<String, DecoderPanelState>,
}

#[derive(Default)]
pub(crate) struct DecoderPanels {
    state: DecoderPanelsState,
    lanes: DerivedLanes,
    tables: DecoderTableRegistry,
    caches: HashMap<String, TableCache>,
    run_generation: u64,
}

impl DecoderPanels {
    pub(crate) fn from_state(state: DecoderPanelsState) -> Self {
        Self {
            state,
            ..Self::default()
        }
    }

    pub(crate) fn state(&self) -> &DecoderPanelsState {
        &self.state
    }

    pub(crate) fn set_run_data(&mut self, lanes: DerivedLanes, tables: DecoderTableRegistry) {
        self.lanes = lanes;
        self.tables = tables;
        self.caches.clear();
        self.run_generation = self.run_generation.wrapping_add(1);
    }

    pub(crate) fn filter_raw_input(&mut self, raw_input: &mut egui::RawInput) -> bool {
        if !self
            .state
            .panels
            .values()
            .any(|state| state.open_menu.is_some())
        {
            return false;
        }
        if !raw_input.events.iter().any(|event| {
            matches!(
                event,
                egui::Event::Key {
                    key: egui::Key::Escape,
                    pressed: true,
                    ..
                }
            )
        }) {
            return false;
        }

        for state in self.state.panels.values_mut() {
            state.open_menu = None;
        }
        raw_input.events.retain(|event| {
            !matches!(
                event,
                egui::Event::Key {
                    key: egui::Key::Escape,
                    ..
                }
            )
        });
        true
    }

    pub(crate) fn show(&mut self, panel_id: &str, ui: &mut egui::Ui) {
        let run_generation = self.run_generation;
        let sources = self.tables.read().clone();
        if sources.is_empty() {
            ui.centered_and_justified(|ui| {
                ui.weak("Run a graph with decoder outputs to populate this table");
            });
            return;
        }

        let selected_elsewhere = self
            .state
            .panels
            .iter()
            .filter(|(other_panel_id, _)| other_panel_id.as_str() != panel_id)
            .filter_map(|(_, state)| state.selected_source.clone())
            .collect::<HashSet<_>>();
        let state = self.state.panels.entry(panel_id.to_owned()).or_default();
        if !sources
            .iter()
            .any(|source| Some(&source.id) == state.selected_source.as_ref())
        {
            state.selected_source = sources
                .iter()
                .find(|source| !selected_elsewhere.contains(&source.id))
                .or_else(|| sources.first())
                .map(|source| source.id.clone());
            state.hidden_columns.clear();
        }
        let selected_id = state.selected_source.clone().unwrap_or_default();
        let Some(source) = sources.iter().find(|source| source.id == selected_id) else {
            return;
        };

        show_decoder_toolbar(panel_id, ui, &sources, source, state);

        let fingerprint = table_fingerprint(source, &self.lanes);
        let cache = self.caches.entry(panel_id.to_owned()).or_default();
        if cache.source_id != source.id {
            cache.row_limit = MAX_TABLE_ROWS;
        }
        if cache.source_id != source.id || cache.fingerprint != fingerprint {
            cache.source_id.clone_from(&source.id);
            cache.fingerprint = fingerprint;
            cache.table = Some(load_table(source, &self.lanes, cache.row_limit));
        }
        let mut load_more = false;
        match cache.table.as_ref().expect("table cache was populated") {
            Ok(table) => {
                load_more = show_table(
                    panel_id,
                    ui,
                    source,
                    state,
                    table,
                    cache.row_limit,
                    run_generation,
                )
            }
            Err(error) => {
                ui.colored_label(Color32::from_rgb(230, 120, 120), error);
            }
        }
        if load_more {
            cache.row_limit = cache.row_limit.saturating_add(MAX_TABLE_ROWS);
            cache.fingerprint.clear();
        }
    }
}

struct TableCache {
    source_id: String,
    fingerprint: Vec<(String, u64, u64)>,
    table: Option<Result<LoadedTable, String>>,
    row_limit: usize,
}

impl Default for TableCache {
    fn default() -> Self {
        Self {
            source_id: String::new(),
            fingerprint: Vec::new(),
            table: None,
            row_limit: MAX_TABLE_ROWS,
        }
    }
}

fn table_fingerprint(source: &DecoderTableSource, lanes: &DerivedLanes) -> Vec<(String, u64, u64)> {
    let lanes = lanes.read();
    source
        .columns
        .iter()
        .filter_map(|column| {
            let lane = lanes
                .iter()
                .find(|lane| lane.name == column.lane.as_str())?;
            let (generation, count) = match &lane.data {
                DerivedLaneData::Annotations(annotations) => (
                    annotations.last().map_or(0, |annotation| annotation.end_ns),
                    annotations.len() as u64,
                ),
                DerivedLaneData::IndexedAnnotations(indexed) => {
                    let metadata = indexed.metadata();
                    (metadata.generation, metadata.total_word_count)
                }
                _ => (0, 0),
            };
            Some((column.lane.as_str().to_owned(), generation, count))
        })
        .collect()
}

fn show_decoder_toolbar(
    panel_id: &str,
    ui: &mut egui::Ui,
    sources: &[DecoderTableSource],
    source: &DecoderTableSource,
    state: &mut DecoderPanelState,
) {
    let mut columns = vec![
        ("sequence".to_owned(), "Sequence".to_owned()),
        ("start".to_owned(), "Start time".to_owned()),
        ("end".to_owned(), "End time".to_owned()),
    ];
    columns.extend(
        source
            .columns
            .iter()
            .map(|column| (format!("output:{}", column.key), column.label.clone())),
    );
    handle_toolbar_keyboard(ui, sources, &columns, state);

    ui.add_space(6.0);
    ui.horizontal_wrapped(|ui| {
        ui.label("Decoder:");
        show_decoder_menu(panel_id, ui, sources, source, state);
        show_format_menu(panel_id, ui, state);
        show_columns_menu(panel_id, ui, &columns, state);
    });
    ui.separator();
}

fn show_decoder_menu(
    panel_id: &str,
    ui: &mut egui::Ui,
    sources: &[DecoderTableSource],
    source: &DecoderTableSource,
    state: &mut DecoderPanelState,
) {
    let button = ui.add(egui::Button::new(&source.label).right_text("▼"));
    if button.clicked() {
        let focus = selected_source_index(sources, state);
        toggle_toolbar_menu(state, ToolbarMenu::Decoder, focus);
    }
    if state.open_menu == Some(ToolbarMenu::Decoder) {
        button.request_focus();
    }
    let mut open = state.open_menu == Some(ToolbarMenu::Decoder);
    let mut selected = None;
    egui::Popup::menu(&button)
        .id(ui.id().with((panel_id, "decoder-source-popup")))
        .open_bool(&mut open)
        .close_behavior(PopupCloseBehavior::CloseOnClickOutside)
        .show(|ui| {
            for (index, candidate) in sources.iter().enumerate() {
                let response = ui.selectable_label(index == state.popup_focus, &candidate.label);
                if response.hovered() {
                    state.popup_focus = index;
                }
                if response.clicked() {
                    selected = Some(candidate.id.clone());
                }
            }
        });
    if let Some(selected) = selected {
        state.selected_source = Some(selected);
        state.hidden_columns.clear();
        open = false;
    }
    finish_toolbar_popup(state, ToolbarMenu::Decoder, open);
}

fn show_format_menu(panel_id: &str, ui: &mut egui::Ui, state: &mut DecoderPanelState) {
    let button = ui.add(egui::Button::new(state.format.label()).right_text("▼"));
    if button.clicked() {
        let focus = selected_format_index(state);
        toggle_toolbar_menu(state, ToolbarMenu::Format, focus);
    }
    if state.open_menu == Some(ToolbarMenu::Format) {
        button.request_focus();
    }
    let mut open = state.open_menu == Some(ToolbarMenu::Format);
    let mut selected = None;
    egui::Popup::menu(&button)
        .id(ui.id().with((panel_id, "decoder-format-popup")))
        .open_bool(&mut open)
        .close_behavior(PopupCloseBehavior::CloseOnClickOutside)
        .show(|ui| {
            for (index, format) in FORMAT_CHOICES.into_iter().enumerate() {
                let response = ui.selectable_label(index == state.popup_focus, format.label());
                if response.hovered() {
                    state.popup_focus = index;
                }
                if response.clicked() {
                    selected = Some(format);
                }
            }
        });
    if let Some(selected) = selected {
        state.format = selected;
        open = false;
    }
    finish_toolbar_popup(state, ToolbarMenu::Format, open);
}

fn show_columns_menu(
    panel_id: &str,
    ui: &mut egui::Ui,
    columns: &[(String, String)],
    state: &mut DecoderPanelState,
) {
    let button = ui.add(egui::Button::new("Columns").right_text("▼"));
    if button.clicked() {
        toggle_toolbar_menu(state, ToolbarMenu::Columns, state.popup_focus);
    }
    if state.open_menu == Some(ToolbarMenu::Columns) {
        button.request_focus();
    }
    let mut open = state.open_menu == Some(ToolbarMenu::Columns);
    egui::Popup::menu(&button)
        .id(ui.id().with((panel_id, "decoder-columns-popup")))
        .open_bool(&mut open)
        .close_behavior(PopupCloseBehavior::CloseOnClickOutside)
        .show(|ui| {
            state.popup_focus = state.popup_focus.min(columns.len().saturating_sub(1));
            for (index, (key, label)) in columns.iter().enumerate() {
                if index == 3 {
                    ui.separator();
                }
                let response = column_toggle(
                    ui,
                    &mut state.hidden_columns,
                    key,
                    label,
                    index == state.popup_focus,
                );
                if response.hovered() {
                    state.popup_focus = index;
                }
            }
        });
    finish_toolbar_popup(state, ToolbarMenu::Columns, open);
}

fn toggle_toolbar_menu(state: &mut DecoderPanelState, menu: ToolbarMenu, focus: usize) {
    if state.open_menu == Some(menu) {
        state.open_menu = None;
    } else {
        state.open_menu = Some(menu);
        state.popup_focus = focus;
    }
}

fn finish_toolbar_popup(state: &mut DecoderPanelState, menu: ToolbarMenu, open: bool) {
    if state.open_menu == Some(menu) && !open {
        state.open_menu = None;
    }
}

fn selected_source_index(sources: &[DecoderTableSource], state: &DecoderPanelState) -> usize {
    sources
        .iter()
        .position(|source| Some(&source.id) == state.selected_source.as_ref())
        .unwrap_or(0)
}

fn selected_format_index(state: &DecoderPanelState) -> usize {
    FORMAT_CHOICES
        .iter()
        .position(|format| *format == state.format)
        .unwrap_or(0)
}

fn column_toggle(
    ui: &mut egui::Ui,
    hidden_columns: &mut HashSet<String>,
    key: &str,
    label: &str,
    focused: bool,
) -> egui::Response {
    let mut visible = !hidden_columns.contains(key);
    let response = ui
        .scope(|ui| {
            if focused {
                let selection = ui.visuals().selection;
                ui.visuals_mut().widgets.inactive.bg_fill = selection.bg_fill;
                ui.visuals_mut().widgets.inactive.fg_stroke = selection.stroke;
            }
            ui.checkbox(&mut visible, label)
        })
        .inner;
    if response.changed() {
        if visible {
            hidden_columns.remove(key);
        } else {
            hidden_columns.insert(key.to_owned());
        }
    }
    response
}

fn handle_toolbar_keyboard(
    ui: &mut egui::Ui,
    sources: &[DecoderTableSource],
    columns: &[(String, String)],
    state: &mut DecoderPanelState,
) {
    let Some(menu) = state.open_menu else {
        return;
    };

    let left = ui.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowLeft));
    let right =
        ui.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowRight));
    if left || right {
        state.open_menu = Some(match (menu, left) {
            (ToolbarMenu::Decoder, true) => ToolbarMenu::Columns,
            (ToolbarMenu::Format, true) => ToolbarMenu::Decoder,
            (ToolbarMenu::Columns, true) => ToolbarMenu::Format,
            (ToolbarMenu::Decoder, false) => ToolbarMenu::Format,
            (ToolbarMenu::Format, false) => ToolbarMenu::Columns,
            (ToolbarMenu::Columns, false) => ToolbarMenu::Decoder,
        });
        state.popup_focus = match state.open_menu {
            Some(ToolbarMenu::Decoder) => selected_source_index(sources, state),
            Some(ToolbarMenu::Format) => selected_format_index(state),
            Some(ToolbarMenu::Columns) | None => 0,
        };
        return;
    }

    let item_count = match menu {
        ToolbarMenu::Decoder => sources.len(),
        ToolbarMenu::Format => FORMAT_CHOICES.len(),
        ToolbarMenu::Columns => columns.len(),
    };
    if item_count == 0 {
        return;
    }
    state.popup_focus = state.popup_focus.min(item_count - 1);
    let down = ui.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown));
    let up = ui.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp));
    if down {
        state.popup_focus = (state.popup_focus + 1) % item_count;
    } else if up {
        state.popup_focus = (state.popup_focus + item_count - 1) % item_count;
    }
    let activate = ui.input_mut(|input| {
        input.consume_key(egui::Modifiers::NONE, egui::Key::Space)
            || input.consume_key(egui::Modifiers::NONE, egui::Key::Enter)
    });
    if !activate {
        return;
    }
    match menu {
        ToolbarMenu::Decoder => {
            state.selected_source = Some(sources[state.popup_focus].id.clone());
            state.hidden_columns.clear();
            state.open_menu = None;
        }
        ToolbarMenu::Format => {
            state.format = FORMAT_CHOICES[state.popup_focus];
            state.open_menu = None;
        }
        ToolbarMenu::Columns => {
            let key = &columns[state.popup_focus].0;
            if !state.hidden_columns.remove(key) {
                state.hidden_columns.insert(key.clone());
            }
        }
    }
}

struct LoadedColumn {
    annotations: Vec<Annotation>,
    lane_format: Option<String>,
}

struct LoadedTable {
    columns: Vec<LoadedColumn>,
    anchor: usize,
    truncated: bool,
}

fn load_table(
    source: &DecoderTableSource,
    lanes: &DerivedLanes,
    row_limit: usize,
) -> Result<LoadedTable, String> {
    let lane_handles = {
        let lanes = lanes.read();
        source
            .columns
            .iter()
            .map(|column| {
                let lane = lanes
                    .iter()
                    .find(|lane| lane.name == column.lane.as_str())
                    .ok_or_else(|| format!("{} is not available", column.label))?;
                let data = match &lane.data {
                    DerivedLaneData::Annotations(annotations) => {
                        ColumnData::Memory(annotations.clone())
                    }
                    DerivedLaneData::IndexedAnnotations(indexed) => {
                        ColumnData::Indexed(indexed.query().clone())
                    }
                    _ => return Err(format!("{} is not tabular data", column.label)),
                };
                Ok((data, lane.word_display_format.clone()))
            })
            .collect::<Result<Vec<_>, String>>()?
    };

    let mut truncated = false;
    let columns = lane_handles
        .into_iter()
        .map(|(data, lane_format)| {
            let annotations = match data {
                ColumnData::Memory(mut annotations) => {
                    if annotations.len() > row_limit {
                        annotations.truncate(row_limit);
                        truncated = true;
                    }
                    annotations
                }
                ColumnData::Indexed(query) => {
                    let metadata = query.metadata();
                    let window = query
                        .exact_window(
                            metadata.first_timestamp_ns.unwrap_or(0),
                            metadata.extent_end_ns.unwrap_or(u64::MAX),
                            row_limit,
                        )
                        .map_err(|error| error.to_string())?;
                    truncated |= !window.complete;
                    window.annotations
                }
            };
            Ok(LoadedColumn {
                annotations,
                lane_format,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    let anchor = source
        .columns
        .iter()
        .position(|column| column.row_anchor)
        .unwrap_or(0);
    Ok(LoadedTable {
        columns,
        anchor,
        truncated,
    })
}

enum ColumnData {
    Memory(Vec<Annotation>),
    Indexed(std::sync::Arc<dyn signal_processing::AnnotationQuery>),
}

fn show_table(
    panel_id: &str,
    ui: &mut egui::Ui,
    source: &DecoderTableSource,
    state: &DecoderPanelState,
    table: &LoadedTable,
    row_limit: usize,
    run_generation: u64,
) -> bool {
    let load_more = if table.truncated {
        ui.horizontal(|ui| {
            ui.colored_label(
                Color32::from_rgb(230, 190, 80),
                format!("Showing the first {row_limit} rows"),
            );
            ui.small_button("Load more").clicked()
        })
        .inner
    } else {
        false
    };
    let rows = &table.columns[table.anchor].annotations;
    if rows.is_empty() {
        ui.centered_and_justified(|ui| ui.weak("No decoded values"));
        return load_more;
    }

    let scroll_id = ui.make_persistent_id((panel_id, source.id.as_str(), run_generation));
    let painted_offset = egui::scroll_area::State::load(ui.ctx(), scroll_id)
        .map_or(egui::Vec2::ZERO, |state| state.offset);
    let row_height = ui
        .text_style_height(&egui::TextStyle::Monospace)
        .max(ui.text_style_height(&egui::TextStyle::Body));
    let scroll_output = egui::ScrollArea::both()
        .id_salt((panel_id, source.id.as_str(), run_generation))
        .auto_shrink([false, false])
        .show_rows(ui, row_height, rows.len() + 1, |ui, range| {
            let start_row = range.start;
            let mut column_cells = Vec::new();
            egui::Grid::new((panel_id, "decoder-table"))
                .striped(true)
                .min_col_width(72.0)
                .start_row(start_row)
                .show(ui, |ui| {
                    for row_index in range {
                        let cells = if row_index == 0 {
                            show_header(ui, source, state)
                        } else {
                            show_row(ui, source, state, table, row_index - 1)
                        };
                        if column_cells.is_empty() {
                            column_cells = cells;
                        }
                    }
                });
            column_cells
        });
    if scroll_output.state.offset.y > 0.5 {
        paint_sticky_header(
            ui,
            scroll_output.inner_rect,
            &scroll_output.inner,
            source,
            state,
            row_height,
        );
    }
    // A resized panel can make the persisted offset exceed the new scroll extent. egui clamps
    // that offset after painting, so request the otherwise-missing frame that uses the clamp.
    if (painted_offset - scroll_output.state.offset).length_sq() > 0.25 {
        ui.ctx().request_repaint();
    }
    load_more
}

fn show_header(
    ui: &mut egui::Ui,
    source: &DecoderTableSource,
    state: &DecoderPanelState,
) -> Vec<egui::Rect> {
    let mut cells = Vec::new();
    if visible(state, "sequence") {
        cells.push(ui.strong("#").rect);
    }
    if visible(state, "start") {
        cells.push(ui.strong("Start").rect);
    }
    if visible(state, "end") {
        cells.push(ui.strong("End").rect);
    }
    for column in &source.columns {
        if visible(state, &format!("output:{}", column.key)) {
            cells.push(ui.strong(&column.label).rect);
        }
    }
    ui.end_row();
    cells
}

fn show_row(
    ui: &mut egui::Ui,
    source: &DecoderTableSource,
    state: &DecoderPanelState,
    table: &LoadedTable,
    index: usize,
) -> Vec<egui::Rect> {
    let mut cells = Vec::new();
    let anchor = table.columns[table.anchor].annotations[index];
    if visible(state, "sequence") {
        cells.push(ui.monospace((index + 1).to_string()).rect);
    }
    if visible(state, "start") {
        cells.push(ui.monospace(format_time_ns(anchor.start_ns)).rect);
    }
    if visible(state, "end") {
        cells.push(ui.monospace(format_time_ns(anchor.end_ns)).rect);
    }
    for (column_index, column) in source.columns.iter().enumerate() {
        if visible(state, &format!("output:{}", column.key)) {
            let loaded = &table.columns[column_index];
            let values = loaded
                .annotations
                .iter()
                .filter(|annotation| {
                    annotation.start_ns >= anchor.start_ns && annotation.start_ns <= anchor.end_ns
                })
                .map(|annotation| format_cell_value(column, annotation.value, state, loaded))
                .collect::<Vec<_>>();
            let text = match &column.cell_mode {
                DecoderTableCellMode::Single => values.first().cloned().unwrap_or_default(),
                DecoderTableCellMode::Joined(separator) => values.join(separator),
            };
            cells.push(ui.monospace(text).rect);
        }
    }
    ui.end_row();
    cells
}

fn paint_sticky_header(
    ui: &egui::Ui,
    viewport: egui::Rect,
    cells: &[egui::Rect],
    source: &DecoderTableSource,
    state: &DecoderPanelState,
    row_height: f32,
) {
    let labels = std::iter::once(("sequence", "#"))
        .chain(std::iter::once(("start", "Start")))
        .chain(std::iter::once(("end", "End")))
        .filter(|(key, _)| visible(state, key))
        .map(|(_, label)| label)
        .chain(
            source
                .columns
                .iter()
                .filter(|column| visible(state, &format!("output:{}", column.key)))
                .map(|column| column.label.as_str()),
        );
    let header_rect = egui::Rect::from_min_size(
        viewport.min,
        egui::vec2(viewport.width(), row_height + ui.spacing().item_spacing.y),
    );
    let painter = ui.painter().with_clip_rect(header_rect);
    painter.rect_filled(header_rect, 0.0, ui.visuals().extreme_bg_color);
    let font = egui::TextStyle::Body.resolve(ui.style());
    let color = ui.visuals().strong_text_color();
    for (cell, label) in cells.iter().zip(labels) {
        painter.text(
            egui::pos2(cell.left(), header_rect.center().y),
            egui::Align2::LEFT_CENTER,
            label,
            font.clone(),
            color,
        );
    }
    painter.line_segment(
        [header_rect.left_bottom(), header_rect.right_bottom()],
        ui.visuals().widgets.noninteractive.bg_stroke,
    );
}

fn format_cell_value(
    column: &DecoderTableColumn,
    value: u64,
    state: &DecoderPanelState,
    loaded: &LoadedColumn,
) -> String {
    let format = state
        .format
        .override_name()
        .or(loaded.lane_format.as_deref());
    let default = AnnotationVisual {
        label: format_value(value, format),
        fill: Color32::from_rgb(88, 58, 28),
        border: Stroke::new(1.0, Color32::from_rgb(215, 140, 60)),
    };
    column
        .renderer
        .annotation_visual(&column.track, value, default)
        .label
}

fn format_value(value: u64, format: Option<&str>) -> String {
    match format.unwrap_or("Hex") {
        "Binary" => format!("{value:b}"),
        "Octal" => format!("{value:o}"),
        "Decimal" => value.to_string(),
        "ASCII" => char::from_u32(value as u32)
            .filter(|character| !character.is_control())
            .map_or_else(|| ".".to_owned(), |character| character.to_string()),
        "Hex + ASCII" => {
            let ascii = char::from_u32(value as u32)
                .filter(|character| !character.is_control())
                .unwrap_or('.');
            format!("{value:02X} '{ascii}'")
        }
        _ => format!("{value:02X}"),
    }
}

fn format_time_ns(ns: u64) -> String {
    if ns >= 1_000_000_000 {
        format!("{:.9} s", ns as f64 / 1_000_000_000.0)
    } else if ns >= 1_000_000 {
        format!("{:.6} ms", ns as f64 / 1_000_000.0)
    } else if ns >= 1_000 {
        format!("{:.3} µs", ns as f64 / 1_000.0)
    } else {
        format!("{ns} ns")
    }
}

fn visible(state: &DecoderPanelState, key: &str) -> bool {
    !state.hidden_columns.contains(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key_event(key: egui::Key) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: Some(key),
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }
    }

    fn source(id: &str) -> DecoderTableSource {
        DecoderTableSource {
            id: id.to_owned(),
            label: id.to_owned(),
            columns: Vec::new(),
        }
    }

    #[test]
    fn data_formats_match_lane_formats() {
        assert_eq!(format_value(0x41, Some("Hex")), "41");
        assert_eq!(format_value(0x41, Some("ASCII")), "A");
        assert_eq!(format_value(0x41, Some("Hex + ASCII")), "41 'A'");
    }

    #[test]
    fn separate_panel_ids_keep_independent_settings() {
        let mut panels = DecoderPanels::default();
        panels
            .state
            .panels
            .entry("panel-1".to_owned())
            .or_default()
            .format = DataFormat::Ascii;
        assert_eq!(
            panels
                .state
                .panels
                .entry("panel-2".to_owned())
                .or_default()
                .format,
            DataFormat::Lane
        );
    }

    #[test]
    fn columns_keyboard_moves_and_toggles_the_focused_item() {
        let context = egui::Context::default();
        context.begin_pass(egui::RawInput {
            events: vec![key_event(egui::Key::ArrowDown), key_event(egui::Key::Space)],
            ..Default::default()
        });
        let mut ui = egui::Ui::new(
            context.clone(),
            egui::Id::new("columns-keyboard-test"),
            egui::UiBuilder::new(),
        );
        let columns = vec![
            ("sequence".to_owned(), "Sequence".to_owned()),
            ("start".to_owned(), "Start".to_owned()),
        ];
        let sources = vec![source("decoder")];
        let mut state = DecoderPanelState {
            open_menu: Some(ToolbarMenu::Columns),
            ..Default::default()
        };

        handle_toolbar_keyboard(&mut ui, &sources, &columns, &mut state);

        assert_eq!(state.popup_focus, 1);
        assert_eq!(state.hidden_columns, HashSet::from(["start".to_owned()]));
        let _ = context.end_pass();
    }

    #[test]
    fn escape_is_filtered_before_widgets_and_closes_all_open_toolbar_popups() {
        let mut panels = DecoderPanels::default();
        panels
            .state
            .panels
            .entry("panel-1".to_owned())
            .or_default()
            .open_menu = Some(ToolbarMenu::Columns);
        panels
            .state
            .panels
            .entry("panel-2".to_owned())
            .or_default()
            .open_menu = Some(ToolbarMenu::Format);
        let mut raw_input = egui::RawInput {
            events: vec![
                key_event(egui::Key::Escape),
                key_event(egui::Key::ArrowDown),
            ],
            ..Default::default()
        };

        assert!(panels.filter_raw_input(&mut raw_input));
        assert!(
            panels
                .state
                .panels
                .values()
                .all(|state| state.open_menu.is_none())
        );
        assert!(!raw_input.events.iter().any(|event| matches!(
            event,
            egui::Event::Key {
                key: egui::Key::Escape,
                ..
            }
        )));
        assert!(raw_input.events.iter().any(|event| matches!(
            event,
            egui::Event::Key {
                key: egui::Key::ArrowDown,
                ..
            }
        )));
    }

    #[test]
    fn left_moves_from_columns_to_the_format_menu() {
        let context = egui::Context::default();
        context.begin_pass(egui::RawInput {
            events: vec![key_event(egui::Key::ArrowLeft)],
            ..Default::default()
        });
        let mut ui = egui::Ui::new(
            context.clone(),
            egui::Id::new("toolbar-left-test"),
            egui::UiBuilder::new(),
        );
        let sources = vec![source("decoder")];
        let mut state = DecoderPanelState {
            open_menu: Some(ToolbarMenu::Columns),
            ..Default::default()
        };

        handle_toolbar_keyboard(&mut ui, &sources, &[], &mut state);

        assert_eq!(state.open_menu, Some(ToolbarMenu::Format));
        let _ = context.end_pass();
    }

    #[test]
    fn format_menu_keyboard_selects_an_item() {
        let context = egui::Context::default();
        context.begin_pass(egui::RawInput {
            events: vec![key_event(egui::Key::ArrowDown), key_event(egui::Key::Enter)],
            ..Default::default()
        });
        let mut ui = egui::Ui::new(
            context.clone(),
            egui::Id::new("format-keyboard-test"),
            egui::UiBuilder::new(),
        );
        let sources = vec![source("decoder")];
        let mut state = DecoderPanelState {
            open_menu: Some(ToolbarMenu::Format),
            ..Default::default()
        };

        handle_toolbar_keyboard(&mut ui, &sources, &[], &mut state);

        assert_eq!(state.format, DataFormat::Ascii);
        assert_eq!(state.open_menu, None);
        let _ = context.end_pass();
    }

    #[test]
    fn decoder_menu_keyboard_selects_an_item() {
        let context = egui::Context::default();
        context.begin_pass(egui::RawInput {
            events: vec![key_event(egui::Key::ArrowDown), key_event(egui::Key::Space)],
            ..Default::default()
        });
        let mut ui = egui::Ui::new(
            context.clone(),
            egui::Id::new("decoder-keyboard-test"),
            egui::UiBuilder::new(),
        );
        let sources = vec![source("first"), source("second")];
        let mut state = DecoderPanelState {
            selected_source: Some("first".to_owned()),
            open_menu: Some(ToolbarMenu::Decoder),
            ..Default::default()
        };

        handle_toolbar_keyboard(&mut ui, &sources, &[], &mut state);

        assert_eq!(state.selected_source.as_deref(), Some("second"));
        assert_eq!(state.open_menu, None);
        let _ = context.end_pass();
    }
}
