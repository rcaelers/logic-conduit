mod builder;
mod reader;
mod storage;
mod types;

use crate::runtime::{CaptureDataSource, CaptureMetadata, CaptureSampledWindow};
use crate::{Error, Result};
use builder::IndexBuilder;
use reader::IndexReader;
use std::path::Path;
use storage::IndexStorage;

pub use types::CaptureIndexProgress;

/// Compatibility wrapper that owns a capture data source and a viewer retrieval reader.
///
/// Build/load orchestration lives here. The actual sampled-window retrieval lives in
/// [`reader::IndexReader`].
pub struct IndexedCaptureReader<S: CaptureDataSource> {
    data_source: S,
    reader: IndexReader<S::Reader>,
}

impl<S> IndexedCaptureReader<S>
where
    S: CaptureDataSource,
{
    pub fn open_data_source(data_source: S) -> Result<Self> {
        Self::open_data_source_with_progress(data_source, |_| {})
    }

    pub fn open_data_source_with_progress<C>(data_source: S, progress: C) -> Result<Self>
    where
        C: FnMut(CaptureIndexProgress),
    {
        let header = data_source.metadata().clone();
        let fingerprint = data_source.fingerprint();
        let index_path = data_source
            .index_path()
            .ok_or_else(|| Error::ParseError("capture source is not indexable".to_string()))?;

        if !IndexStorage::is_valid(&index_path, &header, fingerprint.revision)? {
            IndexBuilder::new(&data_source, &index_path, &header, fingerprint.revision)
                .build(progress)?;
        }

        let storage = IndexStorage::open(index_path, header, fingerprint.revision)?;
        let raw_reader = data_source.open_reader()?;
        let reader = IndexReader::new(storage, raw_reader);

        Ok(Self {
            data_source,
            reader,
        })
    }

    pub fn with_max_cached_leaves(mut self, max: usize) -> Self {
        self.reader = self.reader.with_max_cached_leaves(max);
        self
    }

    pub fn display_name(&self) -> String {
        self.data_source.display_name()
    }

    pub fn index_path(&self) -> &Path {
        self.reader.index_path()
    }

    pub fn header(&self) -> &CaptureMetadata {
        self.reader.header()
    }

    pub fn capture_duration_us(&self) -> f64 {
        self.reader.capture_duration_us()
    }

    pub fn sampled_window(
        &mut self,
        channels: &[usize],
        start_sample: u64,
        end_sample: u64,
        target_points: usize,
    ) -> Result<CaptureSampledWindow> {
        self.reader
            .sampled_window(channels, start_sample, end_sample, target_points)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::{
        BlockCaptureSource, CaptureDataSource, CaptureFingerprint, CaptureMetadata, CaptureSource,
        packed_bit,
    };
    use crate::{Error, Result};
    use std::fs;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[derive(Clone)]
    struct MemoryCaptureDataSource {
        metadata: CaptureMetadata,
        blocks: Arc<Vec<Vec<Arc<[u8]>>>>,
        index_path: PathBuf,
        revision: u64,
    }

    struct MemoryCaptureReader {
        metadata: CaptureMetadata,
        blocks: Arc<Vec<Vec<Arc<[u8]>>>>,
    }

    impl MemoryCaptureDataSource {
        fn new(total_samples: u64, samples_per_block: u64, blocks: Vec<Vec<Vec<u8>>>) -> Self {
            let id = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let total_probes = blocks.len();
            let total_blocks = blocks.first().map_or(0, Vec::len) as u64;
            let blocks = blocks
                .into_iter()
                .map(|channel_blocks| {
                    channel_blocks
                        .into_iter()
                        .map(Arc::<[u8]>::from)
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<_>>();

            Self {
                metadata: CaptureMetadata {
                    total_probes,
                    samplerate: "1 MHz".to_string(),
                    samplerate_hz: 1_000_000.0,
                    sample_period: 0.000_001,
                    total_samples,
                    total_blocks,
                    samples_per_block,
                    probe_names: (0..total_probes)
                        .map(|channel| channel.to_string())
                        .collect(),
                },
                blocks: Arc::new(blocks),
                index_path: std::env::temp_dir().join(format!("capture-index-test-{id}.idx")),
                revision: id as u64,
            }
        }

        fn remove_index(&self) {
            let _ = fs::remove_file(&self.index_path);
        }
    }

    impl CaptureDataSource for MemoryCaptureDataSource {
        type Reader = MemoryCaptureReader;

        fn open_reader(&self) -> Result<Self::Reader> {
            Ok(MemoryCaptureReader {
                metadata: self.metadata.clone(),
                blocks: Arc::clone(&self.blocks),
            })
        }

        fn metadata(&self) -> &CaptureMetadata {
            &self.metadata
        }

        fn fingerprint(&self) -> CaptureFingerprint {
            CaptureFingerprint {
                revision: self.revision,
            }
        }

        fn index_path(&self) -> Option<PathBuf> {
            Some(self.index_path.clone())
        }

        fn display_name(&self) -> String {
            "memory-capture".to_string()
        }
    }

    impl CaptureSource for MemoryCaptureReader {
        fn metadata(&self) -> &CaptureMetadata {
            &self.metadata
        }

        fn read_sample(&mut self, channel: usize, position: u64) -> Result<bool> {
            if position >= self.metadata.total_samples {
                return Err(Error::OutOfBounds(position));
            }
            let block = position / self.metadata.samples_per_block;
            let sample_in_block = (position % self.metadata.samples_per_block) as usize;
            let data = self.read_packed_block(channel, block)?;
            Ok(packed_bit(&data, sample_in_block))
        }
    }

    impl BlockCaptureSource for MemoryCaptureReader {
        fn read_packed_block(&mut self, channel: usize, block: u64) -> Result<Arc<[u8]>> {
            self.blocks
                .get(channel)
                .and_then(|channel_blocks| channel_blocks.get(block as usize))
                .cloned()
                .ok_or(Error::InvalidBlock(block))
        }
    }

    #[test]
    fn chunked_reader_builds_sidecar_and_samples_window() -> Result<()> {
        let mut samples = [0_u8; 16];
        samples[8..16].fill(0xff);

        let source = MemoryCaptureDataSource::new(128, 128, vec![vec![samples.to_vec()]]);
        let mut reader = IndexedCaptureReader::open_data_source(source.clone())?;
        assert!(reader.index_path().exists());
        let window = reader.sampled_window(&[0], 0, 128, 2)?;
        assert_eq!(window.channels.len(), 1);
        assert!(!window.channels[0].initial);
        assert!(window.channels[0].activities.is_empty());
        assert!(window.channels[0].buckets.is_empty());
        assert_eq!(window.channels[0].transitions.len(), 1);
        assert_eq!(window.channels[0].transitions[0].sample, 64);
        assert!(window.channels[0].transitions[0].value);

        source.remove_index();
        Ok(())
    }

    #[test]
    fn zoomed_in_reader_returns_exact_transitions() -> Result<()> {
        let mut samples = [0_u8; 16];
        samples[8..16].fill(0xff);

        let source = MemoryCaptureDataSource::new(128, 128, vec![vec![samples.to_vec()]]);
        let mut reader = IndexedCaptureReader::open_data_source(source.clone())?;
        let window = reader.sampled_window(&[0], 0, 128, 3)?;

        assert_eq!(window.sample_step, 1);
        assert_eq!(window.channels[0].buckets.len(), 0);
        assert_eq!(window.channels[0].transitions.len(), 1);
        assert_eq!(window.channels[0].transitions[0].sample, 64);
        assert!(window.channels[0].transitions[0].value);

        source.remove_index();
        Ok(())
    }

    #[test]
    fn full_file_sampling_uses_block_level_when_zoomed_out() -> Result<()> {
        let source = MemoryCaptureDataSource::new(
            33_554_432,
            16_777_216,
            vec![vec![
                vec![0_u8; 2 * 1024 * 1024],
                vec![0xff_u8; 2 * 1024 * 1024],
            ]],
        );
        let mut reader = IndexedCaptureReader::open_data_source(source.clone())?;
        let window = reader.sampled_window(&[0], 0, 33_554_432, 2)?;

        assert_eq!(window.sample_step, 16_777_216);
        assert_eq!(window.channels[0].buckets.len(), 2);
        assert_eq!(window.channels[0].buckets[0].start_sample, 0);
        assert_eq!(window.channels[0].buckets[0].end_sample, 16_777_216);
        assert!(!window.channels[0].buckets[0].first);
        assert!(!window.channels[0].buckets[0].toggle);
        assert!(!window.channels[0].buckets[0].last);
        assert_eq!(window.channels[0].buckets[1].start_sample, 16_777_216);
        assert_eq!(window.channels[0].buckets[1].end_sample, 33_554_432);
        assert!(window.channels[0].buckets[1].first);
        assert!(window.channels[0].buckets[1].toggle);
        assert!(window.channels[0].buckets[1].last);
        assert!(window.channels[0].transitions.is_empty());

        source.remove_index();
        Ok(())
    }
}
