//! Application-neutral presentation helpers shared by reusable egui widgets.

use egui::Ui;

/// Width reserved for icons and toggle marks in application popup menus.
pub const MENU_ICON_COLUMN_WIDTH: f32 = 22.0;

/// Builds the shared icon-and-label layout used by application popup menus.
///
/// Placeholder colors let the hosting egui button apply hover, active, and
/// disabled colors consistently.
pub fn menu_item_layout_job(ui: &Ui, icon: Option<&str>, label: &str) -> egui::text::LayoutJob {
    let font_id = egui::TextStyle::Button.resolve(ui.style());
    let color = egui::Color32::PLACEHOLDER;
    let format = egui::TextFormat {
        font_id: font_id.clone(),
        color,
        ..Default::default()
    };
    let mut job = egui::text::LayoutJob::default();
    if let Some(icon) = icon {
        let icon_width = ui.ctx().fonts_mut(|fonts| {
            fonts
                .layout_no_wrap(icon.to_owned(), font_id.clone(), color)
                .size()
                .x
        });
        job.append(icon, 0.0, format.clone());
        job.append(
            label,
            (MENU_ICON_COLUMN_WIDTH - icon_width).max(2.0),
            format,
        );
    } else {
        job.append(label, MENU_ICON_COLUMN_WIDTH, format);
    }
    job
}
