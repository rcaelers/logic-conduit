//! Platform-neutral contracts implemented by the native file-backed store and
//! the wasm in-memory store.

use super::platform::StoreResult;
use super::query::AnnotationQuery;
use super::state::LiveStoreSnapshot;
use crate::events::Word;

pub(crate) trait AnnotationStoreBackend:
    AnnotationQuery + Clone + Send + Sync + 'static
{
    fn snapshot(&self) -> LiveStoreSnapshot;
}

pub(crate) trait AnnotationStoreWriterBackend {
    fn append_batch(&mut self, words: &[Word]) -> StoreResult<()>;
    fn finish(&mut self) -> StoreResult<()>;
}
