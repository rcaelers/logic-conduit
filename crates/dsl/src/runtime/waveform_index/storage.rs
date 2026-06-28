use super::types::{
    DIR_ENTRY_SIZE, HEADER_SIZE, IndexHeader, L1_WORDS, L2_WORDS, LeafSummary, MAGIC, RootChunk,
    RootDirEntry,
};
use crate::runtime::CaptureMetadata;
use crate::{Error, Result};
use std::collections::{HashMap, VecDeque};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub(super) struct IndexStorage {
    path: PathBuf,
    header: CaptureMetadata,
    file: File,
    directory: Vec<Vec<RootDirEntry>>,
    root_cache: HashMap<(usize, usize), Arc<RootChunk>>,
    root_cache_order: VecDeque<(usize, usize)>,
    max_cached_roots: usize,
}

impl IndexStorage {
    const DEFAULT_MAX_CACHED_ROOTS: usize = 8;

    pub(super) fn is_valid(
        path: &Path,
        header: &CaptureMetadata,
        source_revision: u64,
    ) -> Result<bool> {
        let Ok(mut file) = File::open(path) else {
            return Ok(false);
        };
        let Ok(index_header) = Self::read_index_header(&mut file) else {
            return Ok(false);
        };
        Ok(Self::validate_index_header(&index_header, header, source_revision).is_ok())
    }

    pub(super) fn open(
        path: PathBuf,
        header: CaptureMetadata,
        source_revision: u64,
    ) -> Result<Self> {
        let mut file = File::open(&path)?;
        let index_header = Self::read_index_header(&mut file)?;
        Self::validate_index_header(&index_header, &header, source_revision)?;
        let roots_per_channel = index_header.roots_per_channel as usize;
        let directory = Self::read_directory(
            &mut file,
            &index_header,
            header.total_probes,
            roots_per_channel,
        )?;

        Ok(Self {
            path,
            header,
            file,
            directory,
            root_cache: HashMap::new(),
            root_cache_order: VecDeque::new(),
            max_cached_roots: Self::DEFAULT_MAX_CACHED_ROOTS,
        })
    }

    pub(super) fn path(&self) -> &Path {
        &self.path
    }

    pub(super) fn header(&self) -> &CaptureMetadata {
        &self.header
    }

    pub(super) fn set_max_cached_roots(&mut self, max_cached_roots: usize) {
        self.max_cached_roots = max_cached_roots.max(1);
        self.trim_root_cache();
    }

    pub(super) fn load_root(
        &mut self,
        channel: usize,
        root_index: usize,
    ) -> Result<Arc<RootChunk>> {
        let key = (channel, root_index);
        if let Some(root) = self.root_cache.get(&key).cloned() {
            self.touch_root_cache_key(key);
            return Ok(root);
        }

        let entry = self
            .directory
            .get(channel)
            .and_then(|roots| roots.get(root_index))
            .copied()
            .ok_or_else(|| Error::ParseError("root chunk index out of bounds".to_string()))?;
        self.file.seek(SeekFrom::Start(entry.offset))?;
        let mut data = vec![0_u8; entry.len as usize];
        self.file.read_exact(&mut data)?;
        let root = Arc::new(Self::deserialize_root_chunk(&data)?);
        self.root_cache.insert(key, Arc::clone(&root));
        self.root_cache_order.push_back(key);
        self.trim_root_cache();
        Ok(root)
    }

    pub(super) fn serialize_root_chunk(root: &RootChunk) -> Vec<u8> {
        let mut out = Vec::new();
        Self::push_u32(&mut out, root.channel as u32);
        Self::push_u32(&mut out, root.root_index as u32);
        Self::push_u64(&mut out, root.first_block);
        Self::push_u32(&mut out, root.block_count);
        Self::push_u32(&mut out, root.leaves.len() as u32);
        Self::push_u64(&mut out, root.root_toggle);
        Self::push_u64(&mut out, root.root_first);
        Self::push_u64(&mut out, root.root_last);
        for leaf in &root.leaves {
            Self::push_u32(&mut out, leaf.valid_samples);
            out.push((leaf.first as u8) | ((leaf.last as u8) << 1) | ((leaf.active as u8) << 2));
            out.extend_from_slice(&[0, 0, 0]);
            if leaf.active {
                Self::push_u64_slice(&mut out, &leaf.l1_toggle);
                Self::push_u64_slice(&mut out, &leaf.l1_last);
                Self::push_u64_slice(&mut out, &leaf.l2_toggle);
                Self::push_u64_slice(&mut out, &leaf.l2_last);
                Self::push_u64(&mut out, leaf.l3_toggle);
                Self::push_u64(&mut out, leaf.l3_last);
            }
        }
        out
    }

    pub(super) fn write_index_header(file: &mut File, header: &IndexHeader) -> Result<()> {
        file.write_all(MAGIC)?;
        Self::write_u32(file, 3)?;
        Self::write_u32(file, HEADER_SIZE as u32)?;
        Self::write_u64(file, header.source_revision)?;
        Self::write_u64(file, header.total_samples)?;
        Self::write_u64(file, header.total_blocks)?;
        Self::write_u64(file, header.samples_per_block)?;
        Self::write_u64(file, header.samplerate_bits)?;
        Self::write_u32(file, header.total_channels)?;
        Self::write_u32(file, header.roots_per_channel)?;
        Self::write_u64(file, header.dir_offset)?;
        Self::write_u64(file, header.payload_offset)?;
        let written = 8 + 4 + 4 + 8 * 7 + 4 * 2;
        file.write_all(&vec![0_u8; HEADER_SIZE as usize - written])?;
        Ok(())
    }

    pub(super) fn write_dir_entry(file: &mut File, entry: &RootDirEntry) -> Result<()> {
        debug_assert_eq!(DIR_ENTRY_SIZE, 48);
        Self::write_u64(file, entry.first_block)?;
        Self::write_u32(file, entry.block_count)?;
        Self::write_u32(file, 0)?;
        Self::write_u64(file, entry.offset)?;
        Self::write_u64(file, entry.len)?;
        Self::write_u64(file, 0)?;
        Self::write_u64(file, 0)?;
        Ok(())
    }

    fn validate_index_header(
        index_header: &IndexHeader,
        header: &CaptureMetadata,
        source_revision: u64,
    ) -> Result<()> {
        if index_header.source_revision != source_revision
            || index_header.total_samples != header.total_samples
            || index_header.total_blocks != header.total_blocks
            || index_header.samples_per_block != header.samples_per_block
            || index_header.samplerate_bits != header.samplerate_hz.to_bits()
            || index_header.total_channels != header.total_probes as u32
            || index_header.roots_per_channel
                != (header.total_blocks as usize).div_ceil(super::types::BLOCKS_PER_ROOT) as u32
        {
            return Err(Error::ParseError("waveform sidecar is stale".to_string()));
        }
        Ok(())
    }

    fn read_index_header(file: &mut File) -> Result<IndexHeader> {
        file.seek(SeekFrom::Start(0))?;
        let mut magic = [0_u8; 8];
        file.read_exact(&mut magic)?;
        if &magic != MAGIC {
            return Err(Error::ParseError(
                "invalid waveform sidecar magic".to_string(),
            ));
        }
        let version = Self::read_u32(file)?;
        if version != 3 {
            return Err(Error::ParseError(
                "unsupported waveform sidecar version".to_string(),
            ));
        }
        let _header_size = Self::read_u32(file)?;
        let source_revision = Self::read_u64(file)?;
        let total_samples = Self::read_u64(file)?;
        let total_blocks = Self::read_u64(file)?;
        let samples_per_block = Self::read_u64(file)?;
        let samplerate_bits = Self::read_u64(file)?;
        let total_channels = Self::read_u32(file)?;
        let roots_per_channel = Self::read_u32(file)?;
        let dir_offset = Self::read_u64(file)?;
        let payload_offset = Self::read_u64(file)?;
        Ok(IndexHeader {
            source_revision,
            total_samples,
            total_blocks,
            samples_per_block,
            samplerate_bits,
            total_channels,
            roots_per_channel,
            dir_offset,
            payload_offset,
        })
    }

    fn read_directory(
        file: &mut File,
        header: &IndexHeader,
        channels: usize,
        roots_per_channel: usize,
    ) -> Result<Vec<Vec<RootDirEntry>>> {
        file.seek(SeekFrom::Start(header.dir_offset))?;
        let mut directory = vec![vec![RootDirEntry::default(); roots_per_channel]; channels];
        for channel_dir in &mut directory {
            for entry in channel_dir {
                *entry = Self::read_dir_entry(file)?;
            }
        }
        Ok(directory)
    }

    fn read_dir_entry(file: &mut File) -> Result<RootDirEntry> {
        let first_block = Self::read_u64(file)?;
        let block_count = Self::read_u32(file)?;
        let _reserved = Self::read_u32(file)?;
        let offset = Self::read_u64(file)?;
        let len = Self::read_u64(file)?;
        let _reserved0 = Self::read_u64(file)?;
        let _reserved1 = Self::read_u64(file)?;
        Ok(RootDirEntry {
            first_block,
            block_count,
            offset,
            len,
        })
    }

    fn deserialize_root_chunk(data: &[u8]) -> Result<RootChunk> {
        let mut cursor = Cursor::new(data);
        let channel = cursor.u32()? as usize;
        let root_index = cursor.u32()? as usize;
        let first_block = cursor.u64()?;
        let block_count = cursor.u32()?;
        let leaf_count = cursor.u32()? as usize;
        let root_toggle = cursor.u64()?;
        let root_first = cursor.u64()?;
        let root_last = cursor.u64()?;
        let mut leaves = Vec::with_capacity(leaf_count);
        for _ in 0..leaf_count {
            let valid_samples = cursor.u32()?;
            let flags = cursor.byte()?;
            cursor.skip(3)?;
            let active = flags & 0b100 != 0;
            let mut leaf = LeafSummary {
                valid_samples,
                first: flags & 0b001 != 0,
                last: flags & 0b010 != 0,
                active,
                l1_toggle: Vec::new(),
                l1_last: Vec::new(),
                l2_toggle: [0; L2_WORDS],
                l2_last: [0; L2_WORDS],
                l3_toggle: 0,
                l3_last: 0,
            };
            if active {
                leaf.l1_toggle = cursor.u64_vec(L1_WORDS)?;
                leaf.l1_last = cursor.u64_vec(L1_WORDS)?;
                leaf.l2_toggle.copy_from_slice(&cursor.u64_vec(L2_WORDS)?);
                leaf.l2_last.copy_from_slice(&cursor.u64_vec(L2_WORDS)?);
                leaf.l3_toggle = cursor.u64()?;
                leaf.l3_last = cursor.u64()?;
            }
            leaves.push(leaf);
        }
        Ok(RootChunk {
            channel,
            root_index,
            first_block,
            block_count,
            root_toggle,
            root_first,
            root_last,
            leaves,
        })
    }

    #[cfg(test)]
    pub(super) fn decode_root_chunk_for_test(data: &[u8]) -> Result<RootChunk> {
        Self::deserialize_root_chunk(data)
    }

    fn touch_root_cache_key(&mut self, key: (usize, usize)) {
        if self
            .root_cache_order
            .back()
            .is_some_and(|existing| *existing == key)
        {
            return;
        }
        self.root_cache_order.retain(|existing| *existing != key);
        self.root_cache_order.push_back(key);
    }

    fn trim_root_cache(&mut self) {
        while self.root_cache.len() > self.max_cached_roots {
            if let Some(key) = self.root_cache_order.pop_front() {
                self.root_cache.remove(&key);
            } else {
                break;
            }
        }
    }

    fn push_u32(out: &mut Vec<u8>, value: u32) {
        out.extend_from_slice(&value.to_le_bytes());
    }

    fn push_u64(out: &mut Vec<u8>, value: u64) {
        out.extend_from_slice(&value.to_le_bytes());
    }

    fn push_u64_slice(out: &mut Vec<u8>, values: &[u64]) {
        for value in values {
            Self::push_u64(out, *value);
        }
    }

    fn read_u32(file: &mut File) -> Result<u32> {
        let mut buf = [0_u8; 4];
        file.read_exact(&mut buf)?;
        Ok(u32::from_le_bytes(buf))
    }

    fn read_u64(file: &mut File) -> Result<u64> {
        let mut buf = [0_u8; 8];
        file.read_exact(&mut buf)?;
        Ok(u64::from_le_bytes(buf))
    }

    fn write_u32(file: &mut File, value: u32) -> Result<()> {
        file.write_all(&value.to_le_bytes())?;
        Ok(())
    }

    fn write_u64(file: &mut File, value: u64) -> Result<()> {
        file.write_all(&value.to_le_bytes())?;
        Ok(())
    }
}

struct Cursor<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    fn byte(&mut self) -> Result<u8> {
        let byte = *self
            .data
            .get(self.pos)
            .ok_or_else(|| Error::ParseError("truncated waveform sidecar".to_string()))?;
        self.pos += 1;
        Ok(byte)
    }

    fn skip(&mut self, count: usize) -> Result<()> {
        if self.pos + count > self.data.len() {
            return Err(Error::ParseError("truncated waveform sidecar".to_string()));
        }
        self.pos += count;
        Ok(())
    }

    fn u32(&mut self) -> Result<u32> {
        let mut buf = [0_u8; 4];
        self.read_exact(&mut buf)?;
        Ok(u32::from_le_bytes(buf))
    }

    fn u64(&mut self) -> Result<u64> {
        let mut buf = [0_u8; 8];
        self.read_exact(&mut buf)?;
        Ok(u64::from_le_bytes(buf))
    }

    fn u64_vec(&mut self, len: usize) -> Result<Vec<u64>> {
        let mut out = Vec::with_capacity(len);
        for _ in 0..len {
            out.push(self.u64()?);
        }
        Ok(out)
    }

    fn read_exact(&mut self, buf: &mut [u8]) -> Result<()> {
        if self.pos + buf.len() > self.data.len() {
            return Err(Error::ParseError("truncated waveform sidecar".to_string()));
        }
        buf.copy_from_slice(&self.data[self.pos..self.pos + buf.len()]);
        self.pos += buf.len();
        Ok(())
    }
}
