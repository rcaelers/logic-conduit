//! Platform-neutral contracts implemented by the native file-backed store and
//! the wasm in-memory store.

use super::query::AnnotationQuery;
use super::store::{IndexedAnnotationStore, LiveStoreSnapshot, StoreResult};
use crate::runtime::Word;

pub(crate) trait AnnotationStoreBackend:
    AnnotationQuery + Clone + Send + Sync + 'static
{
    fn snapshot(&self) -> LiveStoreSnapshot;
}

pub(crate) trait AnnotationStoreWriterBackend {
    fn store(&self) -> IndexedAnnotationStore;
    fn append_batch(&mut self, words: &[Word]) -> StoreResult<()>;
    fn publish_hot_tail(&mut self) -> StoreResult<()>;
    fn finish(&mut self) -> StoreResult<()>;
    fn cancel(&mut self) -> StoreResult<()>;
}
