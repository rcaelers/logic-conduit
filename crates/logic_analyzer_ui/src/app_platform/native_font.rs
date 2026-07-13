pub(super) fn load_symbol_fonts() -> Vec<egui::FontData> {
    symbol_font_paths()
        .iter()
        .filter_map(|path| std::fs::read(path).ok())
        .map(egui::FontData::from_owned)
        .collect()
}

#[cfg(target_os = "macos")]
fn symbol_font_paths() -> &'static [&'static str] {
    &["/System/Library/Fonts/Apple Symbols.ttf"]
}

#[cfg(target_os = "windows")]
fn symbol_font_paths() -> &'static [&'static str] {
    &[r"C:\Windows\Fonts\seguisym.ttf"]
}

#[cfg(target_os = "linux")]
fn symbol_font_paths() -> &'static [&'static str] {
    &[
        "/usr/share/fonts/truetype/noto/NotoSansSymbols2-Regular.ttf",
        "/usr/share/fonts/truetype/noto/NotoSansSymbols-Regular.ttf",
        "/usr/share/fonts/truetype/noto/NotoSansMath-Regular.ttf",
        "/usr/share/fonts/noto/NotoSansSymbols2-Regular.ttf",
        "/usr/share/fonts/noto/NotoSansSymbols-Regular.ttf",
        "/usr/share/fonts/noto/NotoSansMath-Regular.ttf",
        "/usr/share/fonts/google-noto-sans-symbols2-fonts/NotoSansSymbols2-Regular.ttf",
        "/usr/share/fonts/google-noto-sans-symbols-fonts/NotoSansSymbols-Regular.ttf",
        "/usr/share/fonts/google-noto-sans-math-fonts/NotoSansMath-Regular.ttf",
        "/usr/local/share/NotoSansSymbols2-Regular.ttf",
        "/usr/local/share/NotoSansSymbols-Regular.ttf",
        "/usr/local/share/NotoSansMath-Regular.ttf",
    ]
}

#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
fn symbol_font_paths() -> &'static [&'static str] {
    &[]
}
