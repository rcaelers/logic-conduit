use super::storage::IndexWriter;
use super::types::{BlockIndex, BlockLevels, CaptureIndexProgress, SAMPLES_PER_L1_BIT, bit, set_bit};
use crate::runtime::{BlockCaptureSource, CaptureDataSource, CaptureMetadata, packed_bit};
use crate::{Error, Result};
use std::collections::VecDeque;
use std::env;
use std::path::Path;
use std::sync::{Arc, Mutex, mpsc};
use std::thread;

#[derive(Debug, Clone, Copy)]
struct BuildJob {
    channel: usize,
    block: u64,
}

pub(super) struct IndexBuilder<'a, S: CaptureDataSource> {
    data_source: &'a S,
    index_path: &'a Path,
    header: &'a CaptureMetadata,
    source_revision: u64,
}

impl<'a, S> IndexBuilder<'a, S>
where
    S: CaptureDataSource,
{
    pub(super) fn new(
        data_source: &'a S,
        index_path: &'a Path,
        header: &'a CaptureMetadata,
        source_revision: u64,
    ) -> Self {
        Self { data_source, index_path, header, source_revision }
    }

    pub(super) fn build<P>(&self, mut progress: P) -> Result<()>
    where
        P: FnMut(CaptureIndexProgress),
    {
        let total_blocks = self.header.total_blocks as usize;
        let job_count = self.header.total_probes * total_blocks;

        let mut jobs = VecDeque::with_capacity(job_count);
        for channel in 0..self.header.total_probes {
            for block in 0..self.header.total_blocks {
                jobs.push_back(BuildJob { channel, block });
            }
        }

        progress(CaptureIndexProgress { completed_roots: 0, total_roots: job_count });

        let mut chunks = Self::build_chunks_parallel(
            (*self.data_source).clone(),
            self.header,
            jobs,
            &mut progress,
        )?;

        let mut writer =
            IndexWriter::create(self.index_path, self.header, self.source_revision)?;

        for (channel, channel_chunks) in chunks.iter_mut().enumerate() {
            let mut previous_last = None;
            for (block_idx, maybe_leaf) in channel_chunks.iter_mut().enumerate() {
                let Some(leaf) = maybe_leaf else { continue };
                Self::apply_boundary_transition(leaf, previous_last);
                previous_last = Some(leaf.last);
                writer.write_block(channel, block_idx, leaf)?;
            }
        }

        writer.finish()
    }

    fn build_chunks_parallel(
        data_source: S,
        header: &CaptureMetadata,
        jobs: VecDeque<BuildJob>,
        progress: &mut impl FnMut(CaptureIndexProgress),
    ) -> Result<Vec<Vec<Option<BlockIndex>>>> {
        let total_jobs = jobs.len();
        let total_blocks = header.total_blocks as usize;
        let mut chunks = vec![vec![None; total_blocks]; header.total_probes];
        if total_jobs == 0 {
            return Ok(chunks);
        }

        let worker_count = Self::index_worker_count(total_jobs);
        let jobs = Arc::new(Mutex::new(jobs));
        let header = Arc::new(header.clone());
        let (result_tx, result_rx) = mpsc::channel();
        let mut workers = Vec::with_capacity(worker_count);

        for _ in 0..worker_count {
            let jobs = Arc::clone(&jobs);
            let header = Arc::clone(&header);
            let data_source = data_source.clone();
            let result_tx = result_tx.clone();
            workers.push(thread::spawn(move || {
                let worker_result = || -> Result<()> {
                    let mut source = data_source.open_reader()?;
                    loop {
                        let Some(job) = jobs.lock().unwrap().pop_front() else { break };
                        let leaf =
                            Self::build_block_chunk(&mut source, &header, job.channel, job.block)?;
                        if result_tx.send(Ok((job, leaf))).is_err() {
                            break;
                        }
                    }
                    Ok(())
                }();

                if let Err(err) = worker_result {
                    let _ = result_tx.send(Err(err));
                }
            }));
        }
        drop(result_tx);

        let mut received = 0;
        let mut first_error = None;
        while received < total_jobs {
            match result_rx.recv() {
                Ok(Ok((job, leaf))) => {
                    chunks[job.channel][job.block as usize] = Some(leaf);
                    received += 1;
                    progress(CaptureIndexProgress {
                        completed_roots: received,
                        total_roots: total_jobs,
                    });
                }
                Ok(Err(err)) => {
                    first_error = Some(err);
                    break;
                }
                Err(_) => break,
            }
        }

        for worker in workers {
            if worker.join().is_err() && first_error.is_none() {
                first_error = Some(Error::ParseError(
                    "waveform index worker panicked".to_string(),
                ));
            }
        }

        if let Some(err) = first_error {
            return Err(err);
        }
        if received != total_jobs {
            return Err(Error::ParseError(
                "waveform index build did not complete".to_string(),
            ));
        }
        Ok(chunks)
    }

    fn build_block_chunk<R>(
        source: &mut R,
        header: &CaptureMetadata,
        channel: usize,
        block: u64,
    ) -> Result<BlockIndex>
    where
        R: BlockCaptureSource,
    {
        let data = source.read_packed_block(channel, block)?;
        let block_start = block * header.samples_per_block;
        let remaining = header.total_samples.saturating_sub(block_start);
        let valid_samples = ((data.len() as u64) * 8).min(remaining);
        Ok(Self::build_leaf_summary(&data, valid_samples, None))
    }

    fn build_leaf_summary(
        data: &[u8],
        valid_samples: u64,
        previous_last: Option<bool>,
    ) -> BlockIndex {
        let valid_samples = valid_samples.min(u32::MAX as u64) as u32;
        if valid_samples == 0 {
            return BlockIndex { valid_samples, first: false, last: false, levels: None };
        }

        let first = packed_bit(data, 0);
        let last = packed_bit(data, valid_samples as usize - 1);
        let mut entering = previous_last.unwrap_or(first);

        // Allocate directly on the heap to avoid a large stack frame.
        let mut lvl = BlockLevels::zeroed();

        let l1_groups = (valid_samples as usize).div_ceil(64);
        let full_l1_groups = valid_samples as usize / 64;
        for (group, chunk) in data[..full_l1_groups * 8].chunks_exact(8).enumerate() {
            let word = u64::from_le_bytes(chunk.try_into().expect("L1 chunks are exactly 8 bytes"));
            Self::record_l1_group(
                &mut lvl.l1_toggle,
                &mut lvl.l1_last,
                group,
                word,
                64,
                &mut entering,
            );
        }
        if full_l1_groups < l1_groups {
            let (word, valid_bits) =
                Self::partial_l1_word(data, full_l1_groups, valid_samples as usize);
            Self::record_l1_group(
                &mut lvl.l1_toggle,
                &mut lvl.l1_last,
                full_l1_groups,
                word,
                valid_bits,
                &mut entering,
            );
        }

        let l2_groups = l1_groups.div_ceil(64);
        for group in 0..l2_groups {
            if lvl.l1_toggle[group] != 0 {
                set_bit(&mut lvl.l2_toggle[group / 64], group % 64);
            }
            let last_l1_group = ((group + 1) * 64).min(l1_groups).saturating_sub(1);
            if bit(lvl.l1_last[last_l1_group / 64], last_l1_group % 64) {
                set_bit(&mut lvl.l2_last[group / 64], group % 64);
            }
        }

        let l3_groups = l2_groups.div_ceil(64);
        for group in 0..l3_groups {
            if lvl.l2_toggle[group] != 0 {
                set_bit(&mut lvl.l3_toggle, group);
            }
            let last_l2_group = ((group + 1) * 64).min(l2_groups).saturating_sub(1);
            if bit(lvl.l2_last[last_l2_group / 64], last_l2_group % 64) {
                set_bit(&mut lvl.l3_last, group);
            }
        }

        BlockIndex {
            valid_samples,
            first,
            last,
            levels: if lvl.l3_toggle != 0 { Some(lvl) } else { None },
        }
    }

    fn index_worker_count(total_jobs: usize) -> usize {
        let available = thread::available_parallelism()
            .map(|count| count.get())
            .unwrap_or(1);
        let configured = env::var("CAPTURE_INDEX_THREADS")
            .or_else(|_| env::var("DSL_INDEX_THREADS"))
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .filter(|value| *value > 0);

        configured.unwrap_or(available).min(total_jobs).max(1)
    }

    fn record_l1_group(
        l1_toggle: &mut [u64],
        l1_last: &mut [u64],
        group: usize,
        word: u64,
        valid_bits: usize,
        entering: &mut bool,
    ) {
        let first_bit = (word & 1) != 0;
        let boundary_toggle = first_bit != *entering;
        let internal_toggle = Self::l1_word_has_internal_toggle(word, valid_bits);
        if boundary_toggle || internal_toggle {
            set_bit(&mut l1_toggle[group / 64], group % 64);
        }

        *entering = bit(word, valid_bits - 1);
        if *entering {
            set_bit(&mut l1_last[group / 64], group % 64);
        }
    }

    fn partial_l1_word(data: &[u8], group: usize, valid_samples: usize) -> (u64, usize) {
        let sample_start = group * 64;
        let valid_bits = (valid_samples - sample_start).min(64);
        let byte_start = group * 8;
        let mut bytes = [0_u8; 8];
        let available = data.len().saturating_sub(byte_start).min(8);
        if available > 0 {
            bytes[..available].copy_from_slice(&data[byte_start..byte_start + available]);
        }
        let mut word = u64::from_le_bytes(bytes);
        if valid_bits < 64 {
            word &= (1_u64 << valid_bits) - 1;
        }
        (word, valid_bits)
    }

    fn l1_word_has_internal_toggle(word: u64, valid_bits: usize) -> bool {
        if valid_bits <= 1 {
            return false;
        }
        let valid_mask = if valid_bits == 64 { u64::MAX } else { (1_u64 << valid_bits) - 1 };
        let internal_mask = valid_mask & !1_u64;
        (word ^ (word << 1)) & internal_mask != 0
    }

    fn apply_boundary_transition(leaf: &mut BlockIndex, previous_last: Option<bool>) {
        let Some(previous_last) = previous_last else { return };
        if leaf.valid_samples == 0 || previous_last == leaf.first {
            return;
        }

        if leaf.levels.is_none() {
            let mut lvl = BlockLevels::zeroed();
            Self::fill_constant_last_summaries_into(&mut lvl, leaf.first, leaf.valid_samples);
            leaf.levels = Some(lvl);
        }

        let levels = leaf.levels.as_mut().unwrap();
        set_bit(&mut levels.l1_toggle[0], 0);
        set_bit(&mut levels.l2_toggle[0], 0);
        set_bit(&mut levels.l3_toggle, 0);
    }

    fn fill_constant_last_summaries_into(lvl: &mut BlockLevels, first: bool, valid_samples: u32) {
        if !first || valid_samples == 0 {
            return;
        }

        let l1_groups = (valid_samples as usize).div_ceil(SAMPLES_PER_L1_BIT as usize);
        for group in 0..l1_groups {
            set_bit(&mut lvl.l1_last[group / 64], group % 64);
        }

        let l2_groups = l1_groups.div_ceil(64);
        for group in 0..l2_groups {
            set_bit(&mut lvl.l2_last[group / 64], group % 64);
        }

        let l3_groups = l2_groups.div_ceil(64);
        for group in 0..l3_groups {
            set_bit(&mut lvl.l3_last, group);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::storage::{IndexStorage, serialize_leaf};
    use crate::runtime::{
        BlockCaptureSource, CaptureDataSource, CaptureFingerprint, CaptureMetadata, CaptureSource,
    };

    #[derive(Clone)]
    struct TestSource;

    struct TestReader;

    impl CaptureDataSource for TestSource {
        type Reader = TestReader;

        fn open_reader(&self) -> Result<Self::Reader> {
            unreachable!("builder helper tests do not open readers")
        }

        fn metadata(&self) -> &CaptureMetadata {
            unreachable!("builder helper tests do not inspect metadata")
        }

        fn fingerprint(&self) -> CaptureFingerprint {
            unreachable!("builder helper tests do not inspect fingerprints")
        }

        fn index_path(&self) -> Option<std::path::PathBuf> {
            unreachable!("builder helper tests do not inspect paths")
        }

        fn display_name(&self) -> String {
            "test".to_string()
        }
    }

    impl CaptureSource for TestReader {
        fn metadata(&self) -> &CaptureMetadata {
            unreachable!("builder helper tests do not inspect metadata")
        }

        fn read_sample(&mut self, _channel: usize, _position: u64) -> Result<bool> {
            unreachable!("builder helper tests do not read samples")
        }
    }

    impl BlockCaptureSource for TestReader {
        fn read_packed_block(
            &mut self,
            _channel: usize,
            _block: u64,
        ) -> Result<std::sync::Arc<[u8]>> {
            unreachable!("builder helper tests do not read blocks")
        }
    }

    type TestBuilder = IndexBuilder<'static, TestSource>;

    #[test]
    fn constant_leaf_stores_only_root_values() {
        let data = vec![0_u8; 128];
        let leaf = TestBuilder::build_leaf_summary(&data, 1024, None);

        assert!(!leaf.first);
        assert!(!leaf.last);
        assert!(leaf.levels.is_none());
    }

    #[test]
    fn boundary_toggle_activates_constant_leaf() {
        let data = vec![0xff_u8; 128];
        let leaf = TestBuilder::build_leaf_summary(&data, 1024, Some(false));

        assert!(leaf.first);
        assert!(leaf.last);
        let lvl = leaf.levels.as_ref().unwrap();
        assert!(bit(lvl.l1_toggle[0], 0));
        assert!(bit(lvl.l1_last[0], 0));
        assert!(bit(lvl.l2_toggle[0], 0));
        assert!(bit(lvl.l2_last[0], 0));
        assert!(bit(lvl.l3_toggle, 0));
        assert!(bit(lvl.l3_last, 0));
    }

    #[test]
    fn last_value_tracks_group_exit_level() {
        let mut data = vec![0_u8; 16];
        for byte in &mut data[8..16] {
            *byte = 0xff;
        }
        let leaf = TestBuilder::build_leaf_summary(&data, 128, Some(false));

        let lvl = leaf.levels.as_ref().unwrap();
        assert!(!bit(lvl.l1_toggle[0], 0));
        assert!(!bit(lvl.l1_last[0], 0));
        assert!(bit(lvl.l1_toggle[0], 1));
        assert!(bit(lvl.l1_last[0], 1));
        assert!(bit(lvl.l2_toggle[0], 0));
        assert!(bit(lvl.l2_last[0], 0));
        assert!(bit(lvl.l3_toggle, 0));
        assert!(bit(lvl.l3_last, 0));
    }

    #[test]
    fn chunk_round_trips_active_leaf() {
        let leaf = TestBuilder::build_leaf_summary(&[0_u8, 0xff], 16, Some(false));
        let data = serialize_leaf(&leaf);
        let decoded = IndexStorage::decode_leaf_for_test(&data).expect("leaf should decode");
        let lvl = decoded.levels.as_ref().expect("decoded leaf should be active");
        assert!(bit(lvl.l1_toggle[0], 0));
    }

    #[test]
    fn chunk_round_trips_constant_leaf() {
        let leaf = TestBuilder::build_leaf_summary(&[0xff_u8; 8], 64, Some(true));
        let data = serialize_leaf(&leaf);
        let decoded = IndexStorage::decode_leaf_for_test(&data).expect("leaf should decode");
        assert!(decoded.levels.is_none());
        assert!(decoded.first);
        assert!(decoded.last);
    }

    #[test]
    fn word_toggle_detection_handles_boundaries_and_partial_groups() {
        assert!(!TestBuilder::l1_word_has_internal_toggle(0, 64));
        assert!(!TestBuilder::l1_word_has_internal_toggle(u64::MAX, 64));
        assert!(TestBuilder::l1_word_has_internal_toggle(0b10, 2));
        assert!(!TestBuilder::l1_word_has_internal_toggle(0b10, 1));

        let data = [0b0000_1111_u8];
        let leaf = TestBuilder::build_leaf_summary(&data, 8, Some(false));
        let lvl = leaf.levels.as_ref().unwrap();
        assert!(bit(lvl.l1_toggle[0], 0));
        assert!(!bit(lvl.l1_last[0], 0));

        let leaf = TestBuilder::build_leaf_summary(&[0xff], 1, Some(false));
        assert!(leaf.first);
        assert!(leaf.last);
        let lvl = leaf.levels.as_ref().unwrap();
        assert!(bit(lvl.l1_toggle[0], 0));
    }
}
