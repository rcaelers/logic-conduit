//! Decoder-table subscriptions to retained derived data.

use std::sync::Arc;

use logic_analyzer_viewer::{DerivedLaneId, ViewerLaneTrackId};
use node_graph::NodeId;

use super::{DecoderTableColumn, DecoderTableRegistry, DecoderTableSource};
use crate::compiler::ResolvedInputs;

struct PendingSource {
    source_node: NodeId,
    key: String,
    label: String,
    columns: Vec<(usize, DecoderTableColumn)>,
}

pub(crate) fn subscribe_collected_tables(
    collector: NodeId,
    resolved: &ResolvedInputs,
    lane_names: &[(usize, String)],
    registry: &DecoderTableRegistry,
) {
    let mut pending: Vec<PendingSource> = Vec::new();
    for (member, lane_name) in lane_names {
        let Some(input) = resolved.get(0, *member) else {
            continue;
        };
        let Some(table) = &input.decoder_table_column else {
            continue;
        };
        let column = DecoderTableColumn {
            key: table.column_key.clone(),
            label: table.label.clone(),
            lane: DerivedLaneId::new(lane_name.clone()),
            track: ViewerLaneTrackId::new(table.track_key.clone()),
            row_anchor: table.row_anchor,
            cell_mode: table.cell_mode.clone(),
            renderer: Arc::clone(&table.renderer),
        };
        if let Some(source) = pending.iter_mut().find(|source| {
            source.source_node == input.source_node && source.key == table.source_key
        }) {
            source.columns.push((table.order, column));
        } else {
            pending.push(PendingSource {
                source_node: input.source_node,
                key: table.source_key.clone(),
                label: input.source_node_title.clone(),
                columns: vec![(table.order, column)],
            });
        }
    }
    for mut source in pending {
        source.columns.sort_by_key(|(order, _)| *order);
        registry.register(DecoderTableSource {
            id: format!(
                "collector:{}:node:{}:{}",
                collector.0, source.source_node.0, source.key
            ),
            label: source.label,
            columns: source
                .columns
                .into_iter()
                .map(|(_, column)| column)
                .collect(),
        });
    }
}
