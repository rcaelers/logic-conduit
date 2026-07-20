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
//!   directory. Chunks may appear in any order (the build streams them as workers complete);
//!   the directory records each chunk's offset.
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
//! │  PAYLOAD  (all chunks, any order; see directory)    │
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

use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use memmap2::Mmap;

use super::types::{
    BlockIndex, DIR_ENTRY_SIZE, HEADER_SIZE, IndexHeader, L1_WORDS, L2_WORDS, MAGIC, RootDirEntry,
};
use crate::capture::CaptureMetadata;
use crate::{Error, Result};

// Leaf chunks are read zero-copy as native u64 words from the mapped file.
#[cfg(target_endian = "big")]
compile_error!("the waveform index mmap path assumes a little-endian target");

// ---------------------------------------------------------------------------
// IndexWriter — create and populate a new index file
// ---------------------------------------------------------------------------

/// Writes a new index file for one capture source.
///
/// Call [`IndexWriter::create`] to open the file, [`IndexWriter::write_block`] once per
/// (channel, block) pair — in any order, since the directory records each
/// chunk's offset — then [`IndexWriter::finish`] to flush and atomically
/// rename the temp file into place. Dropping the writer without finishing
/// removes the temp file.
pub(crate) struct IndexWriter {
    temp_path: PathBuf,
    final_path: PathBuf,
    file: File,
    directory: Vec<Vec<RootDirEntry>>,
    index_header: IndexHeader,
    finished: bool,
}

impl IndexWriter {
    /// Create a new index file at `path` (written via a `.idx.tmp` sibling until [`finish`]).
    pub(crate) fn create(
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
            finished: false,
        })
    }

    /// Serialize `leaf` and append its chunk to the payload; record the directory entry.
    pub(crate) fn write_block(
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
    pub(crate) fn finish(mut self) -> Result<()> {
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
        fs::rename(&self.temp_path, &self.final_path)?;
        self.finished = true;
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

    fn abort_cleanup(&mut self) {
        if !self.finished {
            let _ = fs::remove_file(&self.temp_path);
        }
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

impl Drop for IndexWriter {
    fn drop(&mut self) {
        self.abort_cleanup();
    }
}

// ---------------------------------------------------------------------------
// IndexReader — read an existing index file
// ---------------------------------------------------------------------------

/// Zero-copy view of one leaf chunk inside the mapped index file.
pub(crate) struct LeafView<'a> {
    #[allow(dead_code)]
    pub(crate) valid_samples: u32,
    pub(crate) first: bool,
    pub(crate) last: bool,
    pub(crate) levels: Option<LevelsView<'a>>,
}

pub(crate) struct LevelsView<'a> {
    pub l1_toggle: &'a [u64],
    pub l1_last: &'a [u64],
    pub l2_toggle: &'a [u64],
    pub l2_last: &'a [u64],
    #[allow(dead_code)]
    pub l3_toggle: u64,
    #[allow(dead_code)]
    pub l3_last: u64,
}

pub(crate) struct IndexReader {
    path: PathBuf,
    header: CaptureMetadata,
    /// Memory-mapped index file; leaf chunks are read directly out of the
    /// mapping, so residency is managed by the OS page cache and no
    /// application-level leaf cache is needed.
    mmap: Mmap,
    directory: Vec<Vec<RootDirEntry>>,
}

impl IndexReader {
    pub(crate) fn is_valid(
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

    pub(crate) fn open(
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
        // SAFETY: the mapping is read-only and the index file is owned by
        // this application; it is atomically replaced (rename), never
        // truncated or rewritten in place while mapped.
        let mmap = unsafe { Mmap::map(&file)? };

        Ok(Self {
            path,
            header,
            mmap,
            directory,
        })
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn header(&self) -> &CaptureMetadata {
        &self.header
    }

    pub(crate) fn load_leaf(&self, channel: usize, block: usize) -> Result<LeafView<'_>> {
        let entry = self
            .directory
            .get(channel)
            .and_then(|blocks| blocks.get(block))
            .copied()
            .ok_or_else(|| Error::ParseError("block index out of bounds".to_string()))?;
        let start = entry.offset as usize;
        let data = start
            .checked_add(entry.len as usize)
            .and_then(|end| self.mmap.get(start..end))
            .ok_or_else(|| Error::ParseError("truncated waveform sidecar".to_string()))?;
        leaf_view(data)
    }

    pub(crate) fn load_root_summary(&self, channel: usize, block: usize) -> Result<RootDirEntry> {
        self.directory
            .get(channel)
            .and_then(|blocks| blocks.get(block))
            .copied()
            .ok_or_else(|| Error::ParseError("block index out of bounds".to_string()))
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

/// Interprets one serialized leaf chunk in place, without copying the level
/// bitmaps. Requires `data` to be 8-byte aligned, which holds for chunks in
/// the mapped index file: the header (96 B), directory entries (40 B), and
/// chunk sizes (8 B constant / 66 584 B active) are all multiples of 8.
fn leaf_view(data: &[u8]) -> Result<LeafView<'_>> {
    let truncated = || Error::ParseError("truncated waveform sidecar".to_string());
    let (chunk_header, payload) = data.split_at_checked(8).ok_or_else(truncated)?;
    let valid_samples = u32::from_le_bytes(
        chunk_header[..4]
            .try_into()
            .expect("chunk header is 8 bytes"),
    );
    let flags = chunk_header[4];

    let levels = if flags & 0b100 != 0 {
        const LEVEL_WORDS: usize = 2 * L1_WORDS + 2 * L2_WORDS + 2;
        let payload = payload.get(..LEVEL_WORDS * 8).ok_or_else(truncated)?;
        // SAFETY: any bit pattern is a valid u64; the slice length is a
        // multiple of 8 and the alignment is checked via the empty prefix.
        let (prefix, words, _) = unsafe { payload.align_to::<u64>() };
        if !prefix.is_empty() || words.len() != LEVEL_WORDS {
            return Err(Error::ParseError(
                "misaligned waveform sidecar chunk".to_string(),
            ));
        }
        let (l1_toggle, rest) = words.split_at(L1_WORDS);
        let (l1_last, rest) = rest.split_at(L1_WORDS);
        let (l2_toggle, rest) = rest.split_at(L2_WORDS);
        let (l2_last, rest) = rest.split_at(L2_WORDS);
        Some(LevelsView {
            l1_toggle,
            l1_last,
            l2_toggle,
            l2_last,
            l3_toggle: rest[0],
            l3_last: rest[1],
        })
    } else {
        None
    };

    Ok(LeafView {
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

#[cfg(test)]
mod tests {
    use super::super::types::{BlockIndex, BlockLevels, bit, set_bit};
    use super::*;

    /// Copies serialized bytes into an 8-byte-aligned buffer, as `leaf_view`
    /// requires (the mapped file provides this alignment naturally).
    fn aligned(data: &[u8]) -> Vec<u64> {
        let mut buf = vec![0_u64; data.len().div_ceil(8)];
        for (word, chunk) in buf.iter_mut().zip(data.chunks(8)) {
            let mut bytes = [0_u8; 8];
            bytes[..chunk.len()].copy_from_slice(chunk);
            *word = u64::from_le_bytes(bytes);
        }
        buf
    }

    fn as_bytes(buf: &[u64], len: usize) -> &[u8] {
        // SAFETY: reinterpreting u64s as bytes is always valid; len is
        // bounded by the buffer size.
        unsafe { std::slice::from_raw_parts(buf.as_ptr().cast::<u8>(), len.min(buf.len() * 8)) }
    }

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
        let buf = aligned(&data);
        let decoded = leaf_view(as_bytes(&buf, data.len())).expect("leaf should decode");
        assert_eq!(decoded.valid_samples, 16);
        assert!(!decoded.first);
        assert!(decoded.last);
        let lvl = decoded
            .levels
            .as_ref()
            .expect("decoded leaf should be active");
        assert!(bit(lvl.l1_toggle[0], 0));
        assert!(bit(lvl.l2_toggle[0], 0));
        assert!(bit(lvl.l3_toggle, 0));
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
        let buf = aligned(&data);
        let decoded = leaf_view(as_bytes(&buf, data.len())).expect("leaf should decode");
        assert!(decoded.levels.is_none());
        assert!(decoded.first);
        assert!(decoded.last);
    }
}
