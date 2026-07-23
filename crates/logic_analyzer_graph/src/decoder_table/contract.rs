use std::fmt;
use std::sync::{Arc, RwLock, RwLockReadGuard};

pub use logic_analyzer_graph_api::node_support::{
    DecoderTableCellMode, DecoderTableColumnPresentation,
};
use logic_analyzer_viewer::{DerivedLaneId, ViewerLaneRenderer, ViewerLaneTrackId};

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
