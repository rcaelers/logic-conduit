mod builder;
mod reader;
mod storage;
mod types;

pub use reader::IndexSampler;
pub use types::CaptureIndexProgress;

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
        let mut reader = IndexSampler::open_data_source(source.clone())?;
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
        let mut reader = IndexSampler::open_data_source(source.clone())?;
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
        let mut reader = IndexSampler::open_data_source(source.clone())?;
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

    #[test]
    fn l2_level_sampling_returns_correct_transitions() -> Result<()> {
        // 33M samples, 2 blocks — target_points=1000 → sample_step≈33554 → L2 level (4096 smp/bit)
        let total_samples = 33_554_432_u64;
        let half = total_samples / 2; // = 16_777_216
        let source = MemoryCaptureDataSource::new(
            total_samples,
            half,
            vec![vec![
                vec![0_u8; 2 * 1024 * 1024],
                vec![0xff_u8; 2 * 1024 * 1024],
            ]],
        );
        let mut reader = IndexSampler::open_data_source(source.clone())?;
        let window = reader.sampled_window(&[0], 0, total_samples, 1000)?;

        assert_eq!(window.sample_step, 4_096); // L2 granularity
        let buckets = &window.channels[0].buckets;
        assert_eq!(buckets.len(), 1000);
        assert!(window.channels[0].transitions.is_empty());

        // All buckets fully inside the first block should be constant-false.
        for b in buckets.iter().filter(|b| b.end_sample <= half) {
            assert!(!b.first, "first-block bucket first=false");
            assert!(!b.toggle, "first-block bucket toggle=false");
            assert!(!b.last, "first-block bucket last=false");
        }

        // At least one bucket must capture the inter-block transition.
        let has_toggle = buckets.iter().any(|b| b.toggle);
        assert!(has_toggle, "some bucket must capture the 0→1 transition");

        // Buckets that start strictly past the block boundary are in the constant-true region.
        // (The bucket starting exactly at `half` covers L2-group-0 of block 1, which carries
        //  the boundary-activation toggle; that bucket is correctly toggle=true.)
        for b in buckets.iter().filter(|b| b.start_sample > half) {
            assert!(!b.toggle, "second-block interior bucket toggle=false, got start={}", b.start_sample);
            assert!(b.last, "second-block interior bucket last=true");
        }

        source.remove_index();
        Ok(())
    }

    #[test]
    fn l3_level_sampling_returns_correct_transitions() -> Result<()> {
        // 33M samples, 2 standard 16M-sample blocks, target_points=100
        // sample_step = 33_554_432 / 100 ≈ 335_544 ≥ 262_144 → L3 level
        let total_samples = 33_554_432_u64;
        let half = total_samples / 2;
        let source = MemoryCaptureDataSource::new(
            total_samples,
            half,
            vec![vec![
                vec![0_u8; 2 * 1024 * 1024],
                vec![0xff_u8; 2 * 1024 * 1024],
            ]],
        );
        let mut reader = IndexSampler::open_data_source(source.clone())?;
        let window = reader.sampled_window(&[0], 0, total_samples, 100)?;

        assert!(window.sample_step >= 262_144, "expected L3 granularity");
        let buckets = &window.channels[0].buckets;
        assert_eq!(buckets.len(), 100);
        assert!(window.channels[0].transitions.is_empty());

        for b in buckets.iter().filter(|b| b.end_sample <= half) {
            assert!(!b.toggle, "first-block bucket toggle=false");
            assert!(!b.last, "first-block bucket last=false");
        }
        let has_toggle = buckets.iter().any(|b| b.toggle);
        assert!(has_toggle, "some bucket must capture the 0→1 transition");
        for b in buckets.iter().filter(|b| b.start_sample > half) {
            assert!(!b.toggle, "second-block interior bucket toggle=false");
            assert!(b.last, "second-block interior bucket last=true");
        }

        source.remove_index();
        Ok(())
    }
}
