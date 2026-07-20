//! Native mmap implementation of the packed-capture backing contract.

use std::sync::Arc;

use super::implementation::{BlockBacking, BlockData};

struct MappedBlockBacking(Arc<memmap2::Mmap>);

impl BlockBacking for MappedBlockBacking {
    fn bytes(&self) -> &[u8] {
        &self.0
    }

    fn shares_backing(&self, other: &dyn BlockBacking) -> bool {
        !self.bytes().is_empty()
            && self.bytes().as_ptr() == other.bytes().as_ptr()
            && self.bytes().len() == other.bytes().len()
    }
}

impl BlockData {
    pub(crate) fn mapped(map: Arc<memmap2::Mmap>, offset: usize, len: usize) -> Self {
        debug_assert!(offset.saturating_add(len) <= map.len());
        Self::from_backing(Arc::new(MappedBlockBacking(map)), offset, len)
    }
}
