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
        BlockCaptureSource, BlockData, CaptureDataSource, CaptureFingerprint, CaptureMetadata,
        CaptureSource, CaptureWaveformSegment, packed_bit,
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
        raw_reads: Arc<AtomicU64>,
    }

    struct MemoryCaptureReader {
        metadata: CaptureMetadata,
        blocks: Arc<Vec<Vec<Arc<[u8]>>>>,
        raw_reads: Arc<AtomicU64>,
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
                raw_reads: Arc::new(AtomicU64::new(0)),
            }
        }

        fn remove_index(&self) {
            let _ = fs::remove_file(&self.index_path);
            let _ = fs::remove_file(self.index_path.with_extension("raw"));
        }

        fn raw_reads(&self) -> u64 {
            self.raw_reads.load(Ordering::Relaxed)
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
                raw_reads: Arc::clone(&self.raw_reads),
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
        fn read_packed_block(&mut self, channel: usize, block: u64) -> Result<BlockData> {
            self.raw_reads.fetch_add(1, Ordering::Relaxed);
            self.blocks
                .get(channel)
                .and_then(|channel_blocks| channel_blocks.get(block as usize))
                .cloned()
                .map(BlockData::from)
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
                // Two adjacent activity points merged into one segment.
                CaptureWaveformSegment::Activity {
                    start_sample: 2_218_524,
                    end_sample: 2_227_524,
                    first: false,
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

    #[test]
    fn next_transition_crosses_every_index_and_block_boundary() -> Result<()> {
        let samples_per_block = 1_u64 << 20;
        let total_samples = 2 * samples_per_block + 123;
        let edges = vec![
            1,
            63,
            64,
            65,
            4_095,
            4_096,
            262_143,
            262_144,
            samples_per_block - 1,
            samples_per_block,
            samples_per_block + 1,
            2 * samples_per_block,
            total_samples - 1,
        ];
        let blocks = single_channel_blocks_from_fn(total_samples, samples_per_block, |sample| {
            edges.partition_point(|edge| *edge <= sample) % 2 == 1
        });
        let source = MemoryCaptureDataSource::new(total_samples, samples_per_block, blocks);
        let mut reader = IndexSampler::open_data_source(source.clone())?;

        let mut position = 0;
        for &expected in &edges {
            let transition = reader
                .next_transition(0, position, total_samples)?
                .expect("expected another transition");
            assert_eq!(transition.sample, expected);
            assert_eq!(
                transition.value,
                edges.partition_point(|edge| *edge <= expected) % 2 == 1
            );
            position = transition.sample;
        }
        assert_eq!(reader.next_transition(0, position, total_samples)?, None);

        // The limit is exclusive, matching the pre-existing exact-window
        // implementation used by EdgeQuery.
        assert_eq!(reader.next_transition(0, 63, 64)?, None);
        assert_eq!(reader.next_transition(0, 63, 65)?.unwrap().sample, 64);

        source.remove_index();
        Ok(())
    }

    #[test]
    fn next_transition_skips_constant_blocks_without_raw_reads() -> Result<()> {
        let samples_per_block = 1_u64 << 20;
        let total_samples = 3 * samples_per_block;
        let blocks = single_channel_blocks_from_fn(total_samples, samples_per_block, |_| false);
        let source = MemoryCaptureDataSource::new(total_samples, samples_per_block, blocks);
        let mut reader = IndexSampler::open_data_source(source.clone())?;
        let reads_after_build = source.raw_reads();

        assert_eq!(reader.next_transition(0, 0, total_samples)?, None);
        assert_eq!(
            source.raw_reads(),
            reads_after_build,
            "constant ranges should be answered entirely from root summaries"
        );

        source.remove_index();
        Ok(())
    }

    struct XorShift(u64);

    impl XorShift {
        fn next(&mut self) -> u64 {
            let mut x = self.0;
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            self.0 = x;
            x
        }

        fn below(&mut self, bound: u64) -> u64 {
            self.next() % bound.max(1)
        }
    }

    fn random_signal(rng: &mut XorShift, total_samples: u64, max_run_magnitude: u64) -> Vec<bool> {
        let mut samples = Vec::with_capacity(total_samples as usize);
        let mut value = rng.below(2) == 1;
        while (samples.len() as u64) < total_samples {
            // Log-uniform run lengths: dense bursts and long idle stretches.
            let magnitude = rng.below(max_run_magnitude);
            let run = 1 + rng.below(1 << magnitude);
            for _ in 0..run.min(total_samples - samples.len() as u64) {
                samples.push(value);
            }
            value = !value;
        }
        samples
    }

    fn blocks_from_samples(samples: &[bool], samples_per_block: u64) -> Vec<Vec<Vec<u8>>> {
        single_channel_blocks_from_fn(samples.len() as u64, samples_per_block, |sample| {
            samples[sample as usize]
        })
    }

    fn has_transition_in(samples: &[bool], start: u64, end: u64) -> bool {
        let start = start.max(1) as usize;
        let end = (end as usize).min(samples.len());
        (start..end).any(|index| samples[index] != samples[index - 1])
    }

    /// Validates an indexed waveform against ground truth:
    /// - segments tile the window contiguously,
    /// - `Level` segments must be truly constant at the claimed value,
    /// - `Activity` segments must contain a real transition within one
    ///   `sample_step` of slack on either side (index granularity smear).
    fn check_waveform_against_samples(
        samples: &[bool],
        window: &crate::runtime::CaptureSampledWindow,
        context: &str,
    ) {
        let channel = &window.channels[0];
        let mut cursor = window.start_sample;
        for segment in &channel.waveform {
            match *segment {
                CaptureWaveformSegment::Level {
                    start_sample,
                    end_sample,
                    value,
                } => {
                    assert_eq!(start_sample, cursor, "gap before level segment ({context})");
                    for sample in start_sample..end_sample {
                        assert_eq!(
                            samples[sample as usize], value,
                            "level segment {start_sample}..{end_sample} claims {value} but \
                             sample {sample} differs ({context})"
                        );
                    }
                    cursor = end_sample;
                }
                CaptureWaveformSegment::Edge { sample, .. } => {
                    assert_eq!(sample, cursor, "edge segment out of place ({context})");
                }
                CaptureWaveformSegment::Activity {
                    start_sample,
                    end_sample,
                    ..
                } => {
                    assert_eq!(
                        start_sample, cursor,
                        "gap before activity segment ({context})"
                    );
                    let slack = window.sample_step;
                    assert!(
                        has_transition_in(
                            samples,
                            start_sample.saturating_sub(slack),
                            end_sample.saturating_add(slack),
                        ),
                        "activity segment {start_sample}..{end_sample} has no nearby \
                         transition ({context})"
                    );
                    cursor = end_sample;
                }
            }
        }
        assert_eq!(
            cursor, window.end_sample,
            "segments do not cover window ({context})"
        );
    }

    fn run_randomized_rounds(
        rng: &mut XorShift,
        rounds: usize,
        block_sizes: &[u64],
        min_samples: u64,
        max_extra_samples: u64,
        max_run_magnitude: u64,
        max_target_points: u64,
    ) -> Result<()> {
        for round in 0..rounds {
            let samples_per_block = block_sizes[rng.below(block_sizes.len() as u64) as usize];
            let total_samples = min_samples + rng.below(max_extra_samples);
            let samples = random_signal(rng, total_samples, max_run_magnitude);
            let blocks = blocks_from_samples(&samples, samples_per_block);
            let source = MemoryCaptureDataSource::new(total_samples, samples_per_block, blocks);
            let mut reader = IndexSampler::open_data_source(source.clone())?;

            for case in 0..25 {
                let start = rng.below(total_samples - 1);
                let len = 1 + rng.below(total_samples - start);
                let end = start + len;
                let target_points = 1 + rng.below(max_target_points) as usize;
                let window = reader.sampled_window(&[0], start, end, target_points)?;
                let context = format!(
                    "round {round} case {case}: spb={samples_per_block} total={total_samples} \
                     window={start}..{end} target={target_points} step={}",
                    window.sample_step
                );

                if window.sample_step == 1 {
                    // Exact path: transitions must match ground truth exactly.
                    assert_eq!(
                        window.channels[0].initial, samples[start as usize],
                        "{context}"
                    );
                    let mut value = samples[start as usize];
                    let mut expected = Vec::new();
                    for sample in (start + 1)..end {
                        if samples[sample as usize] != value {
                            value = samples[sample as usize];
                            expected.push((sample, value));
                        }
                    }
                    let actual: Vec<(u64, bool)> = window.channels[0]
                        .transitions
                        .iter()
                        .map(|transition| (transition.sample, transition.value))
                        .collect();
                    assert_eq!(actual, expected, "{context}");
                } else {
                    check_waveform_against_samples(&samples, &window, &context);
                }
            }

            source.remove_index();
        }
        Ok(())
    }

    #[test]
    fn randomized_indexed_windows_match_ground_truth() -> Result<()> {
        let mut rng = XorShift(0x1234_5678_9abc_def0);
        run_randomized_rounds(
            &mut rng,
            40,
            &[4_096, 16_384, 65_536],
            60_000,
            400_000,
            13,
            200,
        )
    }

    #[test]
    fn randomized_next_transition_matches_ground_truth() -> Result<()> {
        let mut rng = XorShift(0x0ddc_0ffe_e15e_beef);
        for round in 0..16 {
            let samples_per_block = [4_096, 65_536, 1 << 20][rng.below(3) as usize];
            let total_samples = 60_000 + rng.below(300_000);
            let samples = random_signal(&mut rng, total_samples, 18);
            let blocks = blocks_from_samples(&samples, samples_per_block);
            let source = MemoryCaptureDataSource::new(total_samples, samples_per_block, blocks);
            let mut reader = IndexSampler::open_data_source(source.clone())?;

            for case in 0..200 {
                let position = rng.below(total_samples);
                let limit = position + 1 + rng.below(total_samples - position);
                let expected = ((position + 1)..limit)
                    .find(|sample| samples[*sample as usize] != samples[*sample as usize - 1]);
                let actual = reader.next_transition(0, position, limit)?;
                assert_eq!(
                    actual.map(|transition| transition.sample),
                    expected,
                    "round={round} case={case} spb={samples_per_block} \
                     position={position} limit={limit}"
                );
                if let Some(transition) = actual {
                    assert_eq!(transition.value, samples[transition.sample as usize]);
                }
            }

            source.remove_index();
        }
        Ok(())
    }

    /// Large blocks (several L3 groups each) with runs long enough to produce
    /// fully constant blocks — exercises the L3 root-directory path, where
    /// constant blocks have no level bitmaps.
    #[test]
    fn randomized_l3_windows_match_ground_truth() -> Result<()> {
        let mut rng = XorShift(0xfeed_face_cafe_beef);
        run_randomized_rounds(
            &mut rng,
            8,
            &[1 << 20, 1 << 21],
            4_000_000,
            6_000_000,
            23,
            24,
        )
    }

    /// Multi-channel build: chunks stream to disk as workers finish, so the
    /// per-channel boundary chaining (a block whose first sample differs from
    /// its predecessor's last) must survive out-of-order job completion.
    #[test]
    fn multi_channel_build_preserves_block_boundary_transitions() -> Result<()> {
        let samples_per_block = 4_096_u64;
        let total_samples = samples_per_block * 3;
        let channel_blocks = |value_at: fn(u64) -> bool| {
            single_channel_blocks_from_fn(total_samples, samples_per_block, value_at).remove(0)
        };
        let blocks = vec![
            // Flips exactly at every block boundary; each block is constant.
            channel_blocks(|sample| (sample / 4_096) % 2 == 1),
            // Constant high across all blocks.
            channel_blocks(|_| true),
            // Single edge inside the middle block.
            channel_blocks(|sample| sample >= 4_196),
        ];
        let source = MemoryCaptureDataSource::new(total_samples, samples_per_block, blocks);
        let mut reader = IndexSampler::open_data_source(source.clone())?;

        // Block-granularity view: one point per block.
        let window = reader.sampled_window(&[0, 1, 2], 0, total_samples, 3)?;
        assert_eq!(window.sample_step, samples_per_block);
        assert_eq!(
            window.channels[0].waveform,
            vec![
                CaptureWaveformSegment::Level {
                    start_sample: 0,
                    end_sample: 4_096,
                    value: false,
                },
                // Both boundary-flip blocks merge into one activity run that
                // enters and leaves low.
                CaptureWaveformSegment::Activity {
                    start_sample: 4_096,
                    end_sample: 12_288,
                    first: false,
                    last: false,
                },
            ]
        );
        assert_eq!(
            window.channels[1].waveform,
            vec![CaptureWaveformSegment::Level {
                start_sample: 0,
                end_sample: total_samples,
                value: true,
            }]
        );
        assert_eq!(
            window.channels[2].waveform,
            vec![
                CaptureWaveformSegment::Level {
                    start_sample: 0,
                    end_sample: 4_096,
                    value: false,
                },
                CaptureWaveformSegment::Activity {
                    start_sample: 4_096,
                    end_sample: 8_192,
                    first: false,
                    last: true,
                },
                CaptureWaveformSegment::Level {
                    start_sample: 8_192,
                    end_sample: total_samples,
                    value: true,
                },
            ]
        );

        // Exact view across the first boundary sees channel 0's edge at
        // exactly the block start.
        let window = reader.sampled_window(&[0, 1, 2], 4_046, 4_146, 200)?;
        assert_eq!(window.sample_step, 1);
        assert_eq!(window.channels[0].transitions.len(), 1);
        assert_eq!(window.channels[0].transitions[0].sample, 4_096);
        assert!(window.channels[0].transitions[0].value);
        assert!(window.channels[1].initial);
        assert!(window.channels[1].transitions.is_empty());
        assert!(!window.channels[2].initial);
        assert!(window.channels[2].transitions.is_empty());

        source.remove_index();
        Ok(())
    }

    #[test]
    fn raw_block_cache_serves_reopened_exact_windows() -> Result<()> {
        let samples_per_block = 4_096_u64;
        let total_samples = samples_per_block * 2;
        let blocks = single_channel_blocks_from_fn(total_samples, samples_per_block, |sample| {
            (sample / 700) % 2 == 1
        });
        let source = MemoryCaptureDataSource::new(total_samples, samples_per_block, blocks);

        // 3 000 samples with 50 target points takes the exact path and spans
        // both blocks.
        let first_window = {
            let mut reader = IndexSampler::open_data_source(source.clone())?;
            reader.sampled_window(&[0], 3_000, 6_000, 50)?
        };
        assert_eq!(first_window.sample_step, 1);
        let reads_after_first = source.raw_reads();
        assert!(reads_after_first > 0);

        // Reopened sampler must serve the same window from the raw cache
        // without touching the capture source at all.
        let second_window = {
            let mut reader = IndexSampler::open_data_source(source.clone())?;
            reader.sampled_window(&[0], 3_000, 6_000, 50)?
        };
        assert_eq!(source.raw_reads(), reads_after_first);
        assert_eq!(
            first_window.channels[0].initial,
            second_window.channels[0].initial
        );
        assert_eq!(
            first_window.channels[0]
                .transitions
                .iter()
                .map(|transition| (transition.sample, transition.value))
                .collect::<Vec<_>>(),
            second_window.channels[0]
                .transitions
                .iter()
                .map(|transition| (transition.sample, transition.value))
                .collect::<Vec<_>>(),
        );
        assert!(!second_window.channels[0].transitions.is_empty());

        source.remove_index();
        Ok(())
    }

    #[test]
    fn block_level_partial_range_does_not_leak_earlier_block_activity() -> Result<()> {
        let samples_per_block = 16_384_u64;
        let total_samples = samples_per_block * 2;
        let visible_start = samples_per_block / 2;
        let visible_end = visible_start + samples_per_block;
        let blocks =
            single_channel_blocks_from_fn(total_samples, samples_per_block, |sample| sample >= 128);
        let source = MemoryCaptureDataSource::new(total_samples, samples_per_block, blocks);
        let mut reader = IndexSampler::open_data_source(source.clone())?;
        let window = reader.sampled_window(&[0], visible_start, visible_end, 1)?;

        assert_eq!(window.sample_step, samples_per_block);
        assert_eq!(
            window.channels[0].waveform,
            vec![CaptureWaveformSegment::Level {
                start_sample: visible_start,
                end_sample: visible_end,
                value: true,
            }]
        );

        source.remove_index();
        Ok(())
    }
}
