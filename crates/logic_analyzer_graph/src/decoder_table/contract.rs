use std::fmt;
use std::sync::{Arc, RwLock, RwLockReadGuard};

use logic_analyzer_viewer::{DerivedLaneId, ViewerLaneRenderer, ViewerLaneTrackId};

/// How multiple annotations overlapping one table row are combined in a cell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecoderTableCellMode {
    Single,
    Joined(String),
}

/// Protocol-neutral table metadata supplied by one graph-node output.
#[derive(Clone)]
pub struct DecoderTableColumnPresentation {
    pub source_key: String,
    pub column_key: String,
    pub label: String,
    pub order: usize,
    pub row_anchor: bool,
    pub cell_mode: DecoderTableCellMode,
    pub track_key: String,
    pub renderer: Arc<dyn ViewerLaneRenderer>,
}

impl fmt::Debug for DecoderTableColumnPresentation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DecoderTableColumnPresentation")
            .field("source_key", &self.source_key)
            .field("column_key", &self.column_key)
            .field("label", &self.label)
            .field("order", &self.order)
            .field("row_anchor", &self.row_anchor)
            .field("cell_mode", &self.cell_mode)
            .field("track_key", &self.track_key)
            .finish_non_exhaustive()
    }
}

impl DecoderTableColumnPresentation {
    pub fn new(
        source_key: impl Into<String>,
        column_key: impl Into<String>,
        label: impl Into<String>,
        order: usize,
        row_anchor: bool,
        cell_mode: DecoderTableCellMode,
        track_key: impl Into<String>,
        renderer: Arc<dyn ViewerLaneRenderer>,
    ) -> Self {
        Self {
            source_key: source_key.into(),
            column_key: column_key.into(),
            label: label.into(),
            order,
            row_anchor,
            cell_mode,
            track_key: track_key.into(),
            renderer,
        }
    }
}

#[derive(Clone)]
pub struct DecoderTableColumn {
    pub key: String,
    pub label: String,
    pub lane: DerivedLaneId,
    pub track: ViewerLaneTrackId,
    pub row_anchor: bool,
    pub cell_mode: DecoderTableCellMode,
    pub renderer: Arc<dyn ViewerLaneRenderer>,
}

impl fmt::Debug for DecoderTableColumn {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DecoderTableColumn")
            .field("key", &self.key)
            .field("label", &self.label)
            .field("lane", &self.lane)
            .field("track", &self.track)
            .field("row_anchor", &self.row_anchor)
            .field("cell_mode", &self.cell_mode)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone)]
pub struct DecoderTableSource {
    pub id: String,
    pub label: String,
    pub columns: Vec<DecoderTableColumn>,
}

#[derive(Debug, Clone, Default)]
pub struct DecoderTableRegistry {
    inner: Arc<RwLock<Vec<DecoderTableSource>>>,
}

impl DecoderTableRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&self, source: DecoderTableSource) {
        let mut sources = self.inner.write().unwrap();
        if let Some(existing) = sources.iter_mut().find(|existing| existing.id == source.id) {
            *existing = source;
        } else {
            sources.push(source);
        }
    }

    pub fn read(&self) -> RwLockReadGuard<'_, Vec<DecoderTableSource>> {
        self.inner.read().unwrap()
    }

    pub fn clear(&self) {
        self.inner.write().unwrap().clear();
    }
}
