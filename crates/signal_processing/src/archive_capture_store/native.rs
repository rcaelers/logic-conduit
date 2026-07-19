//! Native sparse cache of decompressed archive blocks.
//!
//! Capture archives store their packed sample blocks deflate-compressed,
//! which precludes random access: reading a few kilobytes of samples costs a
//! full block decompression. This cache keeps a sparse sidecar file with one
//! fixed-size slot per (channel, block). A slot is written the first time
//! its block is decompressed and is afterwards served zero-copy from a
//! shared memory map, so only the pages a reader actually touches are
//! faulted in. Disk usage grows only with the regions ever inspected at
//! sample resolution.
//!
//! Layout: 64-byte header, validity bitmap (one bit per slot), then the
//! slots in block-major order (all channels of one block adjacent), starting
//! page-aligned.
//!
//! Consistency: the bitmap is persisted only on clean close, after
//! `sync_data` on the slot data, so a set bit on disk always refers to fully
//! written data. A crash merely loses cache entries; the cache is fully
//! derivable from the archive. Reads go through the map while writes use the
//! file descriptor — coherent on unix targets, which share a unified page
//! cache between `write()` and `MAP_SHARED` mappings.

use std::fs::{File, OpenOptions};
use std::path::Path;
use std::sync::Arc;

use memmap2::{Mmap, MmapOptions};

use crate::Result;
use crate::capture::{BlockData, CaptureMetadata};

const MAGIC: &[u8; 8] = b"CAPRAW01";
const VERSION: u32 = 1;
const HEADER_SIZE: usize = 64;
const SLOT_REGION_ALIGN: usize = 4096;

pub(crate) struct NativeArchiveCaptureStore {
    file: File,
    /// One validity bit per slot; kept in memory and written back on drop.
    bitmap: Vec<u8>,
    bitmap_dirty: bool,
    slots_offset: usize,
    slot_bytes: usize,
    channels: usize,
    total_blocks: u64,
    total_samples: u64,
    samples_per_block: u64,
}

impl NativeArchiveCaptureStore {
    pub(crate) fn open(path: &Path, header: &CaptureMetadata, revision: u64) -> Result<Self> {
        let channels = header.total_probes;
        let total_blocks = header.total_blocks;
        let samples_per_block = header.samples_per_block;
        let slot_bytes = samples_per_block.div_ceil(8) as usize;
        let slots = channels * total_blocks as usize;
        let bitmap_bytes = slots.div_ceil(8);
        let slots_offset = (HEADER_SIZE + bitmap_bytes).next_multiple_of(SLOT_REGION_ALIGN);
        let file_len = slots_offset as u64 + (slots * slot_bytes) as u64;

        let expected_header = encode_header(header, revision);
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;

        let mut map = if file.metadata()?.len() == file_len {
            // SAFETY: read-only mapping of a cache file owned by this
            // application; never truncated while mapped.
            Some(unsafe { Mmap::map(&file)? })
        } else {
            None
        };
        if !map
            .as_ref()
            .is_some_and(|map| map[..HEADER_SIZE] == expected_header)
        {
            map = None;
            // Stale or new: recreate as a sparse file with an empty bitmap.
            file.set_len(0)?;
            file.set_len(file_len)?;
            write_all_at(&file, &expected_header, 0)?;
        }
        let map = match map {
            Some(map) => map,
            // SAFETY: as above.
            None => unsafe { Mmap::map(&file)? },
        };
        let bitmap = map[HEADER_SIZE..HEADER_SIZE + bitmap_bytes].to_vec();

        Ok(Self {
            file,
            bitmap,
            bitmap_dirty: false,
            slots_offset,
            slot_bytes,
            channels,
            total_blocks,
            total_samples: header.total_samples,
            samples_per_block,
        })
    }

    pub(crate) fn get(&self, channel: usize, block: u64) -> Option<BlockData> {
        let slot = self.slot_index(channel, block)?;
        if self.bitmap[slot / 8] & (1 << (slot % 8)) == 0 {
            return None;
        }
        let offset = self.slots_offset + slot * self.slot_bytes;
        let len = self.block_bytes(block);
        // SAFETY: raw-cache slots are immutable after their validity bit is
        // published, the cache file is never truncated while open, and every
        // slot begins at the page-aligned fixed slot size. A per-slot mapping
        // is released when its final SampleBlock view drops, bounding RSS for
        // sequential multi-gigabyte captures without copying the payload.
        let map = unsafe {
            MmapOptions::new()
                .offset(offset as u64)
                .len(len)
                .map(&self.file)
                .ok()?
        };
        Some(BlockData::mapped(Arc::new(map), 0, len))
    }

    /// Stores freshly decompressed block bytes. Failures are ignored: the
    /// cache is an optimization and the caller already holds the data.
    pub(crate) fn put(&mut self, channel: usize, block: u64, data: &[u8]) {
        let Some(slot) = self.slot_index(channel, block) else {
            return;
        };
        let len = self.block_bytes(block);
        if self.bitmap[slot / 8] & (1 << (slot % 8)) != 0 || data.len() < len {
            return;
        }
        let offset = (self.slots_offset + slot * self.slot_bytes) as u64;
        if write_all_at(&self.file, &data[..len], offset).is_ok() {
            self.bitmap[slot / 8] |= 1 << (slot % 8);
            self.bitmap_dirty = true;
        }
    }

    fn slot_index(&self, channel: usize, block: u64) -> Option<usize> {
        (channel < self.channels && block < self.total_blocks)
            .then(|| block as usize * self.channels + channel)
    }

    /// Valid bytes in `block`; the final block of a capture may be shorter
    /// than a full slot.
    fn block_bytes(&self, block: u64) -> usize {
        let remaining = self
            .total_samples
            .saturating_sub(block * self.samples_per_block);
        remaining.min(self.samples_per_block).div_ceil(8) as usize
    }
}

impl Drop for NativeArchiveCaptureStore {
    fn drop(&mut self) {
        if !self.bitmap_dirty {
            return;
        }
        // Slot data must be durable before the bitmap claims it is valid.
        if self.file.sync_data().is_ok() {
            let _ = write_all_at(&self.file, &self.bitmap, HEADER_SIZE as u64);
        }
    }
}

fn encode_header(header: &CaptureMetadata, revision: u64) -> [u8; HEADER_SIZE] {
    let mut out = [0_u8; HEADER_SIZE];
    out[..8].copy_from_slice(MAGIC);
    out[8..12].copy_from_slice(&VERSION.to_le_bytes());
    out[16..24].copy_from_slice(&revision.to_le_bytes());
    out[24..32].copy_from_slice(&header.total_samples.to_le_bytes());
    out[32..40].copy_from_slice(&header.samples_per_block.to_le_bytes());
    out[40..48].copy_from_slice(&header.total_blocks.to_le_bytes());
    out[48..52].copy_from_slice(&(header.total_probes as u32).to_le_bytes());
    out
}

#[cfg(unix)]
fn write_all_at(file: &File, buf: &[u8], offset: u64) -> std::io::Result<()> {
    use std::os::unix::fs::FileExt;
    file.write_all_at(buf, offset)
}

#[cfg(windows)]
fn write_all_at(file: &File, mut buf: &[u8], mut offset: u64) -> std::io::Result<()> {
    use std::os::windows::fs::FileExt;
    while !buf.is_empty() {
        let written = file.seek_write(buf, offset)?;
        buf = &buf[written..];
        offset += written as u64;
    }
    Ok(())
}
