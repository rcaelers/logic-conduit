pub(crate) fn load_bundled_symbol_fonts() -> Vec<egui::FontData> {
    vec![
        egui::FontData::from_static(include_bytes!(
            "../../../../resources/fonts/NotoSansSymbols-Regular.ttf"
        )),
        egui::FontData::from_static(include_bytes!(
            "../../../../resources/fonts/NotoSansSymbols2-Regular.ttf"
        )),
        egui::FontData::from_static(include_bytes!(
            "../../../../resources/fonts/NotoSansMath-Regular.ttf"
        )),
    ]
}

#[cfg(test)]
mod font_tests {
    use std::sync::Arc;

    use super::load_bundled_symbol_fonts;

    #[test]
    fn bundled_fallbacks_cover_application_symbols() {
        let mut definitions = egui::FontDefinitions::default();
        for (index, font_data) in load_bundled_symbol_fonts().into_iter().enumerate() {
            let font_name = format!("bundled-symbols-{index}");
            definitions
                .font_data
                .insert(font_name.clone(), Arc::new(font_data));
            definitions
                .families
                .get_mut(&egui::FontFamily::Proportional)
                .unwrap()
                .push(font_name);
        }

        let ctx = egui::Context::default();
        ctx.set_fonts(definitions);
        ctx.begin_pass(Default::default());
        let _ = ctx.end_pass();
        ctx.begin_pass(Default::default());
        let font_id = egui::FontId::proportional(14.0);
        ctx.fonts_mut(|fonts| {
            const APPLICATION_SYMBOLS: &[char] = &[
                '⇧', '⌘', '⌥', '⇪', '⏎', '↶', '↷', '⌧', '⎘', '⧉', '▣', '▼', '▾',
            ];
            for symbol in APPLICATION_SYMBOLS {
                assert!(
                    fonts.has_glyph(&font_id, *symbol),
                    "bundled font is missing {symbol:?} (U+{:04X})",
                    *symbol as u32
                );
            }
        });
        let _ = ctx.end_pass();
    }
}
