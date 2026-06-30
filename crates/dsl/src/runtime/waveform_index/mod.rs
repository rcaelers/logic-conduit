mod builder;
mod reader;
mod storage;
mod types;

pub use reader::IndexSampler;
pub use types::CaptureIndexProgress;

use types::SAMPLES_PER_L1_BIT;

const EXACT_SCAN_BASE_MAX_SAMPLES: u64 = 4_096;

/// Maximum number of visible samples that should be rendered by scanning the
/// raw capture instead of using the display index.
///
/// The display index's finest granularity is one L1 bit per 64 samples. If a
/// screen pixel covers fewer than that, indexed summaries necessarily widen
/// short pulses. Keeping the exact path active until the viewport is at least
/// one L1 bit per target point makes adjacent zoom levels represent the same
/// waveform semantics.
pub fn exact_window_sample_limit(target_points: usize) -> u64 {
    let target_points = target_points.max(1) as u64;
    EXACT_SCAN_BASE_MAX_SAMPLES.max(target_points.saturating_mul(SAMPLES_PER_L1_BIT))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::{
        BlockCaptureSource, CaptureDataSource, CaptureFingerprint, CaptureMetadata, CaptureSource,
        CaptureWaveformSegment, packed_bit,
    };
    use crate::{Error, Result};
    use std::fs;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEST_SOURCE_ID: AtomicU64 = AtomicU64::new(1);

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
            let id = NEXT_TEST_SOURCE_ID.fetch_add(1, Ordering::Relaxed);
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
                revision: id,
            }
        }

        fn remove_index(&self) {
            let _ = fs::remove_file(&self.index_path);
        }
    }

    fn single_channel_blocks_from_fn(
        total_samples: u64,
        samples_per_block: u64,
        mut value_at: impl FnMut(u64) -> bool,
    ) -> Vec<Vec<Vec<u8>>> {
        let total_blocks = total_samples.div_ceil(samples_per_block);
        let mut blocks = Vec::with_capacity(total_blocks as usize);
        for block in 0..total_blocks {
            let block_start = block * samples_per_block;
            let block_samples = samples_per_block.min(total_samples - block_start);
            let mut data = vec![0_u8; block_samples.div_ceil(8) as usize];
            for local in 0..block_samples {
                if value_at(block_start + local) {
                    data[(local / 8) as usize] |= 1 << (local % 8);
                }
            }
            blocks.push(data);
        }
        vec![blocks]
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
        assert!(window.channels[0].waveform.is_empty());
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
        assert!(window.channels[0].waveform.is_empty());
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
        assert_eq!(
            window.channels[0].waveform,
            vec![
                CaptureWaveformSegment::Level {
                    start_sample: 0,
                    end_sample: 16_777_216,
                    value: false,
                },
                CaptureWaveformSegment::Activity {
                    start_sample: 16_777_216,
                    end_sample: 33_554_432,
                    first: false,
                    last: true,
                },
            ]
        );
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
        assert!(window.channels[0].transitions.is_empty());
        assert_eq!(
            window.channels[0].waveform,
            vec![
                CaptureWaveformSegment::Level {
                    start_sample: 0,
                    end_sample: half,
                    value: false,
                },
                CaptureWaveformSegment::Activity {
                    start_sample: half,
                    end_sample: 16_810_770,
                    first: false,
                    last: true,
                },
                CaptureWaveformSegment::Level {
                    start_sample: 16_810_770,
                    end_sample: total_samples,
                    value: true,
                },
            ]
        );

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
        assert!(window.channels[0].transitions.is_empty());
        assert_eq!(
            window.channels[0].waveform,
            vec![
                CaptureWaveformSegment::Level {
                    start_sample: 0,
                    end_sample: half,
                    value: false,
                },
                CaptureWaveformSegment::Activity {
                    start_sample: half,
                    end_sample: 17_112_760,
                    first: false,
                    last: true,
                },
                CaptureWaveformSegment::Level {
                    start_sample: 17_112_760,
                    end_sample: total_samples,
                    value: true,
                },
            ]
        );

        source.remove_index();
        Ok(())
    }

    #[test]
    fn indexed_unaligned_window_reports_activity_at_edge_pixel() -> Result<()> {
        let total_samples = 4_500_000_u64;
        let samples_per_block = 4096_u64;
        let edge = 2_222_223_u64;
        let blocks = single_channel_blocks_from_fn(total_samples, samples_per_block, |sample| {
            sample >= edge
        });
        let source = MemoryCaptureDataSource::new(total_samples, samples_per_block, blocks);
        let mut reader = IndexSampler::open_data_source(source.clone())?;
        let start = 123_u64;
        let end = total_samples - 77;
        let window = reader.sampled_window(&[0], start, end, 1000)?;

        assert!(window.channels[0].transitions.is_empty());
        assert_eq!(
            window.channels[0].waveform,
            vec![
                CaptureWaveformSegment::Level {
                    start_sample: start,
                    end_sample: 2_218_524,
                    value: false,
                },
                CaptureWaveformSegment::Activity {
                    start_sample: 2_218_524,
                    end_sample: 2_223_024,
                    first: false,
                    last: true,
                },
                CaptureWaveformSegment::Activity {
                    start_sample: 2_223_024,
                    end_sample: 2_227_524,
                    first: true,
                    last: true,
                },
                CaptureWaveformSegment::Level {
                    start_sample: 2_227_524,
                    end_sample: end,
                    value: true,
                },
            ]
        );

        source.remove_index();
        Ok(())
    }
}
