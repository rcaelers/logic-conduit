//! Viewer presentation for binary-decoder output.

use std::sync::Arc;

use logic_analyzer_viewer::DefaultViewerLaneRenderer;

use crate::decoder_table::{DecoderTableCellMode, DecoderTableColumnPresentation};

pub(crate) fn binary_table_column(def_index: usize) -> Option<DecoderTableColumnPresentation> {
    (def_index == 0).then(|| {
        DecoderTableColumnPresentation::new(
            "decoder",
            "data",
            "Data",
            0,
            true,
            DecoderTableCellMode::Single,
            "primary",
            Arc::new(DefaultViewerLaneRenderer),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_are_an_explicit_table_source() {
        assert!(binary_table_column(1).is_none());
        let table = binary_table_column(0).unwrap();
        assert_eq!(table.source_key, "decoder");
        assert!(table.row_anchor);
    }
}
