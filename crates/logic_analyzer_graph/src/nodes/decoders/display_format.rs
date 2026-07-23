use node_graph::EnumValue;

const DISPLAY_FORMATS: &[&str] = &["Hex", "Binary", "Octal", "Decimal", "ASCII", "Hex + ASCII"];

pub(crate) fn default_display_format() -> EnumValue {
    EnumValue::new(0, DISPLAY_FORMATS)
}
