//! # Terminology
//!
//! - **Sample**: a single 1-bit logic level reading at one point in time, on one channel.
//! - **Block**: the unit of raw capture data. The capture source divides the sample stream into
//!   fixed-size blocks (e.g. 16 M samples each). One block = one packed bit-array from the source.
//! - **Chunk**: the serialized index payload for one (channel, block) pair written into the index
//!   file. A chunk contains `valid_samples`, flags, and — when the block is active — the mipmap
//!   bitmaps (`BlockLevels`). The directory maps each (channel, block) to its chunk's byte offset
//!   and length.
//! - **Payload**: the region of the index file that holds all the chunks, after the header and
//!   directory. Chunks are written in channel-major order.
//!
//! # Index file format  (magic `CAPIDX06`, version 6)
//!
//! ```text
//! ┌─────────────────────────────────────────────────────┐
//! │  HEADER  (96 bytes, offset 0)                       │
//! │    magic            [u8; 8]  = b"CAPIDX06"          │
//! │    version          u32      = 6                    │
//! │    header_size      u32      = 96                   │
//! │    source_revision  u64                             │
//! │    total_samples    u64                             │
//! │    total_blocks     u64                             │
//! │    samples_per_block u64                            │
//! │    samplerate_bits  u64  (f64::to_bits of Hz)       │
//! │    total_channels   u32                             │
//! │    blocks_per_channel u32                           │
//! │    dir_offset       u64  = 96                       │
//! │    payload_offset   u64  = 96 + channels            │
//! │                               * blocks * 40         │
//! │    _padding         to fill 96 bytes                │
//! ├─────────────────────────────────────────────────────┤
//! │  DIRECTORY  (channels × blocks × 40 bytes)          │
//! │  channel-major order; one entry per (channel,block) │
//! │    offset     u64  (byte offset of chunk in file)   │
//! │    len        u64  (byte length of chunk)           │
//! │    flags      u8   bit0=toggle  bit1=first          │
//! │                        bit2=last                    │
//! │    _padding   [u8; 7]                               │
//! │    l3_toggle  u64  (1 bit per 262 144-sample group: │
//! │                     any transition in group?)        │
//! │    l3_last    u64  (last sample value of group)     │
//! ├─────────────────────────────────────────────────────┤
//! │  PAYLOAD  (all chunks, channel-major order)         │
//! │  Each chunk covers one (channel, block) pair:       │
//! │    valid_samples  u32                               │
//! │    flags          u8  bit0=first  bit1=last         │
//! │                           bit2=active               │
//! │    _padding       [u8; 3]                           │
//! │    [only when active:]                              │
//! │      l1_toggle  [u64; 4096]  (1 bit per 64 samples) │
//! │      l1_last    [u64; 4096]                         │
//! │      l2_toggle  [u64;   64]  (1 bit per 4 096 smp)  │
//! │      l2_last    [u64;   64]                         │
//! │      l3_toggle  u64          (1 bit per 262 144 smp)│
//! │      l3_last    u64                                 │
//! └─────────────────────────────────────────────────────┘
//! ```

use super::types::{
    BlockIndex, BlockLevels, DIR_ENTRY_SIZE, HEADER_SIZE, IndexHeader, MAGIC, RootDirEntry,
};
use crate::runtime::CaptureMetadata;
use crate::{Error, Result};
use std::collections::{HashMap, VecDeque};
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

// ---------------------------------------------------------------------------
// IndexWriter — create and populate a new index file
// ---------------------------------------------------------------------------

/// Writes a new index file for one capture source.
///
/// Call [`IndexWriter::create`] to open the file, [`IndexWriter::write_block`] once per
/// (channel, block) pair in channel-major order, then [`IndexWriter::finish`] to flush
/// and atomically rename the temp file into place.
pub(super) struct IndexWriter {
    temp_path: PathBuf,
    final_path: PathBuf,
    file: File,
    directory: Vec<Vec<RootDirEntry>>,
    index_header: IndexHeader,
}

impl IndexWriter {
    /// Create a new index file at `path` (written via a `.idx.tmp` sibling until [`finish`]).
    pub(super) fn create(
        path: &Path,
        capture_header: &CaptureMetadata,
        source_revision: u64,
    ) -> Result<Self> {
        let temp_path = path.with_extension("idx.tmp");
        let channels = capture_header.total_probes;
        let total_blocks = capture_header.total_blocks as usize;
        let dir_offset = HEADER_SIZE;
        let payload_offset = dir_offset + (channels * total_blocks) as u64 * DIR_ENTRY_SIZE;

        let index_header = IndexHeader {
            source_revision,
            total_samples: capture_header.total_samples,
            total_blocks: capture_header.total_blocks,
            samples_per_block: capture_header.samples_per_block,
            samplerate_bits: capture_header.samplerate_hz.to_bits(),
            total_channels: channels as u32,
            blocks_per_channel: total_blocks as u32,
            dir_offset,
            payload_offset,
        };

        let mut file = File::create(&temp_path)?;
        // Reserve space for header + directory; filled in during finish().
        file.write_all(&vec![0_u8; payload_offset as usize])?;
        file.seek(SeekFrom::Start(payload_offset))?;

        Ok(Self {
            temp_path,
            final_path: path.to_path_buf(),
            file,
            directory: vec![vec![RootDirEntry::default(); total_blocks]; channels],
            index_header,
        })
    }

    /// Serialize `leaf` and append its chunk to the payload; record the directory entry.
    pub(super) fn write_block(
        &mut self,
        channel: usize,
        block: usize,
        leaf: &BlockIndex,
    ) -> Result<()> {
        let offset = self.file.stream_position()?;
        let payload = serialize_leaf(leaf);
        self.file.write_all(&payload)?;
        self.directory[channel][block] = RootDirEntry {
            offset,
            len: payload.len() as u64,
            toggle: leaf.levels.is_some(),
            first: leaf.first,
            last: leaf.last,
            l3_toggle: leaf.levels.as_ref().map_or(0, |l| l.l3_toggle),
            l3_last: leaf.levels.as_ref().map_or(0, |l| l.l3_last),
        };
        Ok(())
    }

    /// Write the header and directory, sync, and atomically rename into place.
    pub(super) fn finish(mut self) -> Result<()> {
        self.file.seek(SeekFrom::Start(0))?;
        Self::write_header(&mut self.file, &self.index_header)?;
        self.file
            .seek(SeekFrom::Start(self.index_header.dir_offset))?;
        for channel_dir in &self.directory {
            for entry in channel_dir {
                Self::write_dir_entry(&mut self.file, entry)?;
            }
        }
        self.file.sync_all()?;
        drop(self.file);
        fs::rename(&self.temp_path, &self.final_path)?;
        Ok(())
    }

    fn write_header(file: &mut File, header: &IndexHeader) -> Result<()> {
        file.write_all(MAGIC)?;
        write_u32(file, 6)?;
        write_u32(file, HEADER_SIZE as u32)?;
        write_u64(file, header.source_revision)?;
        write_u64(file, header.total_samples)?;
        write_u64(file, header.total_blocks)?;
        write_u64(file, header.samples_per_block)?;
        write_u64(file, header.samplerate_bits)?;
        write_u32(file, header.total_channels)?;
        write_u32(file, header.blocks_per_channel)?;
        write_u64(file, header.dir_offset)?;
        write_u64(file, header.payload_offset)?;
        let written = 8 + 4 + 4 + 8 * 7 + 4 * 2;
        file.write_all(&vec![0_u8; HEADER_SIZE as usize - written])?;
        Ok(())
    }

    fn write_dir_entry(file: &mut File, entry: &RootDirEntry) -> Result<()> {
        debug_assert_eq!(DIR_ENTRY_SIZE, 40);
        write_u64(file, entry.offset)?;
        write_u64(file, entry.len)?;
        let flags = (entry.toggle as u8) | ((entry.first as u8) << 1) | ((entry.last as u8) << 2);
        file.write_all(&[flags, 0, 0, 0, 0, 0, 0, 0])?;
        write_u64(file, entry.l3_toggle)?;
        write_u64(file, entry.l3_last)?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// IndexReader — read an existing index file
// ---------------------------------------------------------------------------

pub(super) struct IndexReader {
    path: PathBuf,
    header: CaptureMetadata,
    file: File,
    directory: Vec<Vec<RootDirEntry>>,
    leaf_cache: HashMap<(usize, usize), Arc<BlockIndex>>,
    leaf_cache_order: VecDeque<(usize, usize)>,
    max_cached_leaves: usize,
}

impl IndexReader {
    const DEFAULT_MAX_CACHED_LEAVES: usize = 8;

    pub(super) fn is_valid(
        path: &Path,
        header: &CaptureMetadata,
        source_revision: u64,
    ) -> Result<bool> {
        let Ok(mut file) = File::open(path) else {
            return Ok(false);
        };
        let Ok(index_header) = Self::read_header(&mut file) else {
            return Ok(false);
        };
        Ok(Self::validate_header(&index_header, header, source_revision).is_ok())
    }

    pub(super) fn open(
        path: PathBuf,
        header: CaptureMetadata,
        source_revision: u64,
    ) -> Result<Self> {
        let mut file = File::open(&path)?;
        let index_header = Self::read_header(&mut file)?;
        Self::validate_header(&index_header, &header, source_revision)?;
        let blocks_per_channel = index_header.blocks_per_channel as usize;
        let directory = Self::read_directory(
            &mut file,
            &index_header,
            header.total_probes,
            blocks_per_channel,
        )?;

        Ok(Self {
            path,
            header,
            file,
            directory,
            leaf_cache: HashMap::new(),
            leaf_cache_order: VecDeque::new(),
            max_cached_leaves: Self::DEFAULT_MAX_CACHED_LEAVES,
        })
    }

    pub(super) fn path(&self) -> &Path {
        &self.path
    }

    pub(super) fn header(&self) -> &CaptureMetadata {
        &self.header
    }

    pub(super) fn set_max_cached_leaves(&mut self, max: usize) {
        self.max_cached_leaves = max.max(1);
        self.trim_leaf_cache();
    }

    pub(super) fn load_leaf(&mut self, channel: usize, block: usize) -> Result<Arc<BlockIndex>> {
        let key = (channel, block);
        if let Some(leaf) = self.leaf_cache.get(&key).cloned() {
            self.touch_leaf_cache_key(key);
            return Ok(leaf);
        }

        let entry = self
            .directory
            .get(channel)
            .and_then(|blocks| blocks.get(block))
            .copied()
            .ok_or_else(|| Error::ParseError("block index out of bounds".to_string()))?;
        self.file.seek(SeekFrom::Start(entry.offset))?;
        let mut data = vec![0_u8; entry.len as usize];
        self.file.read_exact(&mut data)?;
        let leaf = Arc::new(deserialize_leaf(&data)?);
        self.leaf_cache.insert(key, Arc::clone(&leaf));
        self.leaf_cache_order.push_back(key);
        self.trim_leaf_cache();
        Ok(leaf)
    }

    pub(super) fn load_root_summary(&self, channel: usize, block: usize) -> Result<RootDirEntry> {
        self.directory
            .get(channel)
            .and_then(|blocks| blocks.get(block))
            .copied()
            .ok_or_else(|| Error::ParseError("block index out of bounds".to_string()))
    }

    #[cfg(test)]
    pub(super) fn decode_leaf_for_test(data: &[u8]) -> Result<BlockIndex> {
        deserialize_leaf(data)
    }

    fn validate_header(
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
            || index_header.blocks_per_channel != header.total_blocks as u32
        {
            return Err(Error::ParseError("waveform sidecar is stale".to_string()));
        }
        Ok(())
    }

    fn read_header(file: &mut File) -> Result<IndexHeader> {
        file.seek(SeekFrom::Start(0))?;
        let mut magic = [0_u8; 8];
        file.read_exact(&mut magic)?;
        if &magic != MAGIC {
            return Err(Error::ParseError(
                "invalid waveform sidecar magic".to_string(),
            ));
        }
        let version = read_u32(file)?;
        if version != 6 {
            return Err(Error::ParseError(
                "unsupported waveform sidecar version".to_string(),
            ));
        }
        let _header_size = read_u32(file)?;
        Ok(IndexHeader {
            source_revision: read_u64(file)?,
            total_samples: read_u64(file)?,
            total_blocks: read_u64(file)?,
            samples_per_block: read_u64(file)?,
            samplerate_bits: read_u64(file)?,
            total_channels: read_u32(file)?,
            blocks_per_channel: read_u32(file)?,
            dir_offset: read_u64(file)?,
            payload_offset: read_u64(file)?,
        })
    }

    fn read_directory(
        file: &mut File,
        header: &IndexHeader,
        channels: usize,
        blocks_per_channel: usize,
    ) -> Result<Vec<Vec<RootDirEntry>>> {
        file.seek(SeekFrom::Start(header.dir_offset))?;
        let mut directory = vec![vec![RootDirEntry::default(); blocks_per_channel]; channels];
        for channel_dir in &mut directory {
            for entry in channel_dir {
                *entry = Self::read_dir_entry(file)?;
            }
        }
        Ok(directory)
    }

    fn read_dir_entry(file: &mut File) -> Result<RootDirEntry> {
        let offset = read_u64(file)?;
        let len = read_u64(file)?;
        let mut flags_buf = [0_u8; 8];
        file.read_exact(&mut flags_buf)?;
        let flags = flags_buf[0];
        let l3_toggle = read_u64(file)?;
        let l3_last = read_u64(file)?;
        Ok(RootDirEntry {
            offset,
            len,
            toggle: flags & 0b001 != 0,
            first: flags & 0b010 != 0,
            last: flags & 0b100 != 0,
            l3_toggle,
            l3_last,
        })
    }

    fn touch_leaf_cache_key(&mut self, key: (usize, usize)) {
        if self
            .leaf_cache_order
            .back()
            .is_some_and(|existing| *existing == key)
        {
            return;
        }
        self.leaf_cache_order.retain(|existing| *existing != key);
        self.leaf_cache_order.push_back(key);
    }

    fn trim_leaf_cache(&mut self) {
        while self.leaf_cache.len() > self.max_cached_leaves {
            if let Some(key) = self.leaf_cache_order.pop_front() {
                self.leaf_cache.remove(&key);
            } else {
                break;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Chunk codec — shared by IndexWriter (serialize) and IndexReader (deserialize)
// ---------------------------------------------------------------------------

fn serialize_leaf(leaf: &BlockIndex) -> Vec<u8> {
    let active = leaf.levels.is_some();
    let mut out = Vec::new();
    push_u32(&mut out, leaf.valid_samples);
    out.push((leaf.first as u8) | ((leaf.last as u8) << 1) | ((active as u8) << 2));
    out.extend_from_slice(&[0, 0, 0]);
    if let Some(levels) = &leaf.levels {
        push_u64_slice(&mut out, &levels.l1_toggle);
        push_u64_slice(&mut out, &levels.l1_last);
        push_u64_slice(&mut out, &levels.l2_toggle);
        push_u64_slice(&mut out, &levels.l2_last);
        push_u64(&mut out, levels.l3_toggle);
        push_u64(&mut out, levels.l3_last);
    }
    out
}

fn deserialize_leaf(data: &[u8]) -> Result<BlockIndex> {
    let mut cursor = Cursor::new(data);
    let valid_samples = cursor.u32()?;
    let flags = cursor.byte()?;
    cursor.skip(3)?;
    let levels = if flags & 0b100 != 0 {
        let mut lvl = BlockLevels::zeroed();
        cursor.u64_slice(&mut lvl.l1_toggle)?;
        cursor.u64_slice(&mut lvl.l1_last)?;
        cursor.u64_slice(&mut lvl.l2_toggle)?;
        cursor.u64_slice(&mut lvl.l2_last)?;
        lvl.l3_toggle = cursor.u64()?;
        lvl.l3_last = cursor.u64()?;
        Some(lvl)
    } else {
        None
    };
    Ok(BlockIndex {
        valid_samples,
        first: flags & 0b001 != 0,
        last: flags & 0b010 != 0,
        levels,
    })
}

// ---------------------------------------------------------------------------
// Low-level I/O helpers
// ---------------------------------------------------------------------------

fn push_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn push_u64(out: &mut Vec<u8>, value: u64) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn push_u64_slice(out: &mut Vec<u8>, values: &[u64]) {
    for value in values {
        push_u64(out, *value);
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

// ---------------------------------------------------------------------------
// Cursor — byte-slice reader for deserialization
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::super::types::{BlockIndex, BlockLevels, bit, set_bit};
    use super::*;

    #[test]
    fn chunk_round_trips_active_leaf() {
        let mut lvl = BlockLevels::zeroed();
        set_bit(&mut lvl.l1_toggle[0], 0);
        set_bit(&mut lvl.l2_toggle[0], 0);
        set_bit(&mut lvl.l3_toggle, 0);
        let leaf = BlockIndex {
            valid_samples: 16,
            first: false,
            last: true,
            levels: Some(lvl),
        };
        let data = serialize_leaf(&leaf);
        let decoded = IndexReader::decode_leaf_for_test(&data).expect("leaf should decode");
        let lvl = decoded
            .levels
            .as_ref()
            .expect("decoded leaf should be active");
        assert!(bit(lvl.l1_toggle[0], 0));
    }

    #[test]
    fn chunk_round_trips_constant_leaf() {
        let leaf = BlockIndex {
            valid_samples: 64,
            first: true,
            last: true,
            levels: None,
        };
        let data = serialize_leaf(&leaf);
        let decoded = IndexReader::decode_leaf_for_test(&data).expect("leaf should decode");
        assert!(decoded.levels.is_none());
        assert!(decoded.first);
        assert!(decoded.last);
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

    fn u64_slice(&mut self, out: &mut [u64]) -> Result<()> {
        for word in out.iter_mut() {
            *word = self.u64()?;
        }
        Ok(())
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
