use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use super::config::PersistentStoreConfig;
use super::format::{BlockDirectoryEntry, DATA_HEADER_SIZE, DataFileHeader, FORMAT_VERSION};
use super::platform::{StoreError, StoreResult};
use super::presence::{WordPresenceIndex, WordSummaryRecord};

const INDEX_MAGIC: &[u8; 8] = b"DWRIDX1\0";
const MANIFEST_MAGIC: &[u8; 8] = b"DWRMAN1\0";
const INDEX_VERSION: u32 = 2;
const MANIFEST_VERSION: u32 = 1;
const INDEX_HEADER_SIZE: usize = 96;
const INDEX_RECORD_SIZE: usize = 64;
const SUMMARY_RECORD_SIZE: usize = 40;
const MANIFEST_SIZE: usize = 96;

const DATA_FILE_NAME: &str = "words.dwd";
pub(crate) const INDEX_FILE_NAME: &str = "words.dwi";
pub(crate) const MANIFEST_FILE_NAME: &str = "manifest.dwm";

#[derive(Debug)]
pub(crate) struct PersistentIndex {
    pub(crate) directory: Vec<BlockDirectoryEntry>,
    pub(crate) presence: WordPresenceIndex,
    pub(crate) committed_word_count: u64,
    pub(crate) committed_data_len: u64,
    pub(crate) first_timestamp_ns: Option<u64>,
    pub(crate) last_timestamp_ns: Option<u64>,
}

pub(crate) struct Publication<'a> {
    pub directory: &'a [BlockDirectoryEntry],
    pub presence: &'a WordPresenceIndex,
    pub committed_word_count: u64,
    pub committed_data_len: u64,
    pub first_timestamp_ns: Option<u64>,
    pub last_timestamp_ns: Option<u64>,
    pub created_unix_ns: u64,
}

#[derive(Debug, Clone, Copy)]
struct Manifest {
    cache_key: [u8; 32],
    data_len: u64,
    index_len: u64,
    word_count: u64,
    created_unix_ns: u64,
    accessed_unix_ns: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PersistentCacheStats {
    pub entries: usize,
    pub total_bytes: u64,
    pub removed_entries: usize,
    pub removed_bytes: u64,
}

pub fn cleanup_cache(
    directory: &Path,
    max_total_bytes: u64,
    pinned_keys: &[[u8; 32]],
) -> StoreResult<PersistentCacheStats> {
    fs::create_dir_all(directory)?;
    remove_stale_temporaries(directory);
    let pinned: Vec<String> = pinned_keys.iter().map(hex_key).collect();
    let mut entries = Vec::new();
    let mut stats = PersistentCacheStats::default();
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let path = entry.path();
        let manifest_path = path.join(MANIFEST_FILE_NAME);
        let manifest = fs::read(&manifest_path)
            .map_err(StoreError::from)
            .and_then(|bytes| decode_manifest(&bytes));
        let Ok(manifest) = manifest else {
            let bytes = directory_size(&path);
            fs::remove_dir_all(&path)?;
            stats.removed_entries += 1;
            stats.removed_bytes = stats.removed_bytes.saturating_add(bytes);
            continue;
        };
        let bytes = directory_size(&path);
        let name = entry.file_name().to_string_lossy().into_owned();
        stats.entries += 1;
        stats.total_bytes = stats.total_bytes.saturating_add(bytes);
        entries.push((manifest.accessed_unix_ns, name, path, bytes));
    }
    if stats.total_bytes > max_total_bytes {
        entries.sort_by_key(|entry| entry.0);
        for (_, name, path, bytes) in entries {
            if stats.total_bytes <= max_total_bytes {
                break;
            }
            if pinned.iter().any(|pinned| pinned == &name) {
                continue;
            }
            fs::remove_dir_all(path)?;
            stats.entries -= 1;
            stats.total_bytes = stats.total_bytes.saturating_sub(bytes);
            stats.removed_entries += 1;
            stats.removed_bytes = stats.removed_bytes.saturating_add(bytes);
        }
    }
    Ok(stats)
}

pub fn clear_cache(directory: &Path) -> StoreResult<PersistentCacheStats> {
    let mut stats = PersistentCacheStats::default();
    if !directory.exists() {
        return Ok(stats);
    }
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let path = entry.path();
        let bytes = if entry.file_type()?.is_dir() {
            directory_size(&path)
        } else {
            entry.metadata()?.len()
        };
        if path.is_dir() {
            fs::remove_dir_all(path)?;
            stats.removed_entries += 1;
        } else {
            fs::remove_file(path)?;
        }
        stats.removed_bytes = stats.removed_bytes.saturating_add(bytes);
    }
    Ok(stats)
}

pub fn clear_cache_entry(config: &PersistentStoreConfig) -> StoreResult<PersistentCacheStats> {
    let path = cache_directory(config);
    if !path.exists() {
        return Ok(PersistentCacheStats::default());
    }
    let bytes = directory_size(&path);
    fs::remove_dir_all(path)?;
    Ok(PersistentCacheStats {
        removed_entries: 1,
        removed_bytes: bytes,
        ..PersistentCacheStats::default()
    })
}

pub(crate) fn cache_directory(config: &PersistentStoreConfig) -> PathBuf {
    config.directory.join(hex_key(&config.cache_key))
}

pub(crate) fn data_path(config: &PersistentStoreConfig) -> PathBuf {
    cache_directory(config).join(DATA_FILE_NAME)
}

pub(crate) fn publish_index_and_manifest(
    config: &PersistentStoreConfig,
    publication: Publication<'_>,
) -> StoreResult<(PathBuf, PathBuf)> {
    let cache_dir = cache_directory(config);
    fs::create_dir_all(&cache_dir)?;
    remove_stale_temporaries(&cache_dir);

    let index_bytes = encode_index(
        config.cache_key,
        publication.directory,
        publication.presence.leaves(),
        publication.committed_word_count,
        publication.committed_data_len,
        publication.first_timestamp_ns,
        publication.last_timestamp_ns,
    )?;
    let index_tmp = cache_dir.join(format!("{INDEX_FILE_NAME}.tmp"));
    write_synced(&index_tmp, &index_bytes)?;

    let now = unix_ns();
    let manifest = Manifest {
        cache_key: config.cache_key,
        data_len: publication.committed_data_len,
        index_len: index_bytes.len() as u64,
        word_count: publication.committed_word_count,
        created_unix_ns: publication.created_unix_ns,
        accessed_unix_ns: now,
    };
    let manifest_tmp = cache_dir.join(format!("{MANIFEST_FILE_NAME}.tmp"));
    write_synced(&manifest_tmp, &encode_manifest(manifest))?;
    Ok((index_tmp, manifest_tmp))
}

pub(crate) fn finish_publication(
    config: &PersistentStoreConfig,
    data_tmp: &Path,
    index_tmp: &Path,
    manifest_tmp: &Path,
) -> StoreResult<()> {
    let cache_dir = cache_directory(config);
    let data_final = cache_dir.join(DATA_FILE_NAME);
    let index_final = cache_dir.join(INDEX_FILE_NAME);
    let manifest_final = cache_dir.join(MANIFEST_FILE_NAME);
    remove_if_exists(&manifest_final)?;
    remove_if_exists(&data_final)?;
    remove_if_exists(&index_final)?;
    fs::rename(data_tmp, &data_final)?;
    fs::rename(index_tmp, &index_final)?;
    fs::rename(manifest_tmp, &manifest_final)?;
    sync_directory(&cache_dir)?;
    Ok(())
}

pub(crate) fn open(config: &PersistentStoreConfig) -> StoreResult<Option<PersistentIndex>> {
    let cache_dir = cache_directory(config);
    let manifest_path = cache_dir.join(MANIFEST_FILE_NAME);
    if !manifest_path.is_file() {
        return Ok(None);
    }
    let result = open_inner(config, &manifest_path);
    match result {
        Ok(index) => {
            touch_manifest(&manifest_path)?;
            Ok(Some(index))
        }
        Err(error) => {
            let _ = fs::remove_dir_all(&cache_dir);
            Err(error)
        }
    }
}

fn open_inner(
    config: &PersistentStoreConfig,
    manifest_path: &Path,
) -> StoreResult<PersistentIndex> {
    let manifest = decode_manifest(&fs::read(manifest_path)?)?;
    if manifest.cache_key != config.cache_key {
        return Err(StoreError::Persistent("manifest cache key mismatch".into()));
    }
    let cache_dir = cache_directory(config);
    let data_path = cache_dir.join(DATA_FILE_NAME);
    let index_path = cache_dir.join(INDEX_FILE_NAME);
    if fs::metadata(&data_path)?.len() != manifest.data_len
        || fs::metadata(&index_path)?.len() != manifest.index_len
    {
        return Err(StoreError::Persistent(
            "persistent file length mismatch".into(),
        ));
    }
    let mut header_bytes = [0u8; DATA_HEADER_SIZE];
    File::open(&data_path)?.read_exact(&mut header_bytes)?;
    let data_header = DataFileHeader::from_bytes(&header_bytes)?;
    if data_header.cache_key_prefix != config.cache_key[..16] {
        return Err(StoreError::Persistent("data cache key mismatch".into()));
    }
    let index = decode_index(&fs::read(index_path)?, config.cache_key)?;
    if index.committed_word_count != manifest.word_count
        || index.committed_data_len != manifest.data_len
    {
        return Err(StoreError::Persistent(
            "manifest/index metadata mismatch".into(),
        ));
    }
    validate_directory(&index.directory, manifest.data_len)?;
    Ok(index)
}

fn encode_index(
    cache_key: [u8; 32],
    directory: &[BlockDirectoryEntry],
    summaries: &[WordSummaryRecord],
    word_count: u64,
    data_len: u64,
    first_timestamp_ns: Option<u64>,
    last_timestamp_ns: Option<u64>,
) -> StoreResult<Vec<u8>> {
    let directory_bytes = directory
        .len()
        .checked_mul(INDEX_RECORD_SIZE)
        .ok_or_else(|| StoreError::Persistent("index size overflow".into()))?;
    let summary_bytes = summaries
        .len()
        .checked_mul(SUMMARY_RECORD_SIZE)
        .ok_or_else(|| StoreError::Persistent("summary index size overflow".into()))?;
    let index_len = INDEX_HEADER_SIZE
        .checked_add(directory_bytes)
        .and_then(|length| length.checked_add(summary_bytes))
        .ok_or_else(|| StoreError::Persistent("index size overflow".into()))?;
    let summary_count = u32::try_from(summaries.len())
        .map_err(|_| StoreError::Persistent("summary count exceeds u32".into()))?;
    validate_summaries(directory, summaries, word_count)?;
    let mut bytes = vec![0u8; index_len];
    bytes[..8].copy_from_slice(INDEX_MAGIC);
    put_u32(&mut bytes, 8, INDEX_VERSION);
    put_u32(&mut bytes, 12, FORMAT_VERSION);
    bytes[16..48].copy_from_slice(&cache_key);
    put_u64(&mut bytes, 48, directory.len() as u64);
    put_u64(&mut bytes, 56, word_count);
    put_optional_u64(&mut bytes, 64, first_timestamp_ns);
    put_optional_u64(&mut bytes, 72, last_timestamp_ns);
    put_u64(&mut bytes, 80, data_len);
    put_u32(&mut bytes, 92, summary_count);
    for (index, entry) in directory.iter().enumerate() {
        let offset = INDEX_HEADER_SIZE + index * INDEX_RECORD_SIZE;
        put_u64(&mut bytes, offset, entry.sequence);
        put_u64(&mut bytes, offset + 8, entry.first_timestamp_ns);
        put_u64(&mut bytes, offset + 16, entry.last_timestamp_ns);
        put_u64(&mut bytes, offset + 24, entry.data_offset);
        put_u32(&mut bytes, offset + 32, entry.block_len);
        put_u32(&mut bytes, offset + 36, entry.word_count);
        bytes[offset + 40] = entry.value_bytes;
        bytes[offset + 41] = entry.flags;
    }
    let summaries_offset = INDEX_HEADER_SIZE + directory_bytes;
    for (index, summary) in summaries.iter().enumerate() {
        let offset = summaries_offset + index * SUMMARY_RECORD_SIZE;
        put_u64(&mut bytes, offset, summary.start_ns);
        put_u64(&mut bytes, offset + 8, summary.end_ns);
        put_u64(&mut bytes, offset + 16, summary.word_count);
        put_u64(&mut bytes, offset + 24, summary.first_block);
        put_u32(&mut bytes, offset + 32, summary.block_count);
    }
    let checksum = crate::crc32c::block_checksum(&bytes, 88);
    put_u32(&mut bytes, 88, checksum);
    Ok(bytes)
}

fn decode_index(bytes: &[u8], cache_key: [u8; 32]) -> StoreResult<PersistentIndex> {
    if bytes.len() < INDEX_HEADER_SIZE || &bytes[..8] != INDEX_MAGIC {
        return Err(StoreError::Persistent(
            "invalid persistent index header".into(),
        ));
    }
    if get_u32(bytes, 8)? != INDEX_VERSION || get_u32(bytes, 12)? != FORMAT_VERSION {
        return Err(StoreError::Persistent(
            "unsupported persistent index version".into(),
        ));
    }
    if bytes[16..48] != cache_key {
        return Err(StoreError::Persistent("index cache key mismatch".into()));
    }
    let expected_checksum = get_u32(bytes, 88)?;
    if crate::crc32c::block_checksum(bytes, 88) != expected_checksum {
        return Err(StoreError::Persistent(
            "persistent index checksum mismatch".into(),
        ));
    }
    let block_count = usize::try_from(get_u64(bytes, 48)?)
        .map_err(|_| StoreError::Persistent("block count exceeds usize".into()))?;
    let summary_count = get_u32(bytes, 92)? as usize;
    let directory_bytes = block_count
        .checked_mul(INDEX_RECORD_SIZE)
        .ok_or_else(|| StoreError::Persistent("persistent index record size overflow".into()))?;
    let summary_bytes = summary_count
        .checked_mul(SUMMARY_RECORD_SIZE)
        .ok_or_else(|| StoreError::Persistent("persistent summary record size overflow".into()))?;
    let expected_len = INDEX_HEADER_SIZE
        .checked_add(directory_bytes)
        .and_then(|length| length.checked_add(summary_bytes))
        .ok_or_else(|| StoreError::Persistent("persistent index size overflow".into()))?;
    if bytes.len() != expected_len {
        return Err(StoreError::Persistent(
            "persistent index length mismatch".into(),
        ));
    }
    let mut directory = Vec::with_capacity(block_count);
    for index in 0..block_count {
        let offset = INDEX_HEADER_SIZE + index * INDEX_RECORD_SIZE;
        let entry = BlockDirectoryEntry {
            sequence: get_u64(bytes, offset)?,
            first_timestamp_ns: get_u64(bytes, offset + 8)?,
            last_timestamp_ns: get_u64(bytes, offset + 16)?,
            data_offset: get_u64(bytes, offset + 24)?,
            block_len: get_u32(bytes, offset + 32)?,
            word_count: get_u32(bytes, offset + 36)?,
            value_bytes: bytes[offset + 40],
            flags: bytes[offset + 41],
        };
        directory.push(entry);
    }
    let mut summaries = Vec::with_capacity(summary_count);
    let summaries_offset = INDEX_HEADER_SIZE + directory_bytes;
    for index in 0..summary_count {
        let offset = summaries_offset + index * SUMMARY_RECORD_SIZE;
        summaries.push(WordSummaryRecord {
            start_ns: get_u64(bytes, offset)?,
            end_ns: get_u64(bytes, offset + 8)?,
            word_count: get_u64(bytes, offset + 16)?,
            first_block: get_u64(bytes, offset + 24)?,
            block_count: get_u32(bytes, offset + 32)?,
        });
    }
    let committed_word_count = get_u64(bytes, 56)?;
    validate_summaries(&directory, &summaries, committed_word_count)?;
    let mut presence = WordPresenceIndex::new();
    for summary in summaries {
        presence.push(summary);
    }
    Ok(PersistentIndex {
        directory,
        presence,
        committed_word_count,
        committed_data_len: get_u64(bytes, 80)?,
        first_timestamp_ns: get_optional_u64(bytes, 64)?,
        last_timestamp_ns: get_optional_u64(bytes, 72)?,
    })
}

fn validate_summaries(
    directory: &[BlockDirectoryEntry],
    summaries: &[WordSummaryRecord],
    expected_word_count: u64,
) -> StoreResult<()> {
    let mut words_per_block = vec![0u64; directory.len()];
    let mut previous_start = None;
    for summary in summaries {
        let block = usize::try_from(summary.first_block)
            .map_err(|_| StoreError::Persistent("summary block exceeds usize".into()))?;
        if summary.word_count == 0
            || summary.start_ns > summary.end_ns
            || summary.block_count != 1
            || block >= directory.len()
            || previous_start.is_some_and(|start| summary.start_ns < start)
        {
            return Err(StoreError::Persistent(
                "invalid persistent presence summary".into(),
            ));
        }
        words_per_block[block] = words_per_block[block].saturating_add(summary.word_count);
        previous_start = Some(summary.start_ns);
    }
    if words_per_block
        .iter()
        .zip(directory)
        .any(|(&count, entry)| count != u64::from(entry.word_count))
        || words_per_block
            .iter()
            .copied()
            .fold(0u64, u64::saturating_add)
            != expected_word_count
    {
        return Err(StoreError::Persistent(
            "presence summary word count mismatch".into(),
        ));
    }
    Ok(())
}

fn validate_directory(directory: &[BlockDirectoryEntry], data_len: u64) -> StoreResult<()> {
    let mut expected_offset = DATA_HEADER_SIZE as u64;
    for (index, entry) in directory.iter().enumerate() {
        if entry.sequence != index as u64
            || entry.data_offset != expected_offset
            || entry.word_count == 0
            || entry.first_timestamp_ns > entry.last_timestamp_ns
        {
            return Err(StoreError::Persistent("invalid block directory".into()));
        }
        expected_offset = expected_offset
            .checked_add(u64::from(entry.block_len))
            .ok_or_else(|| StoreError::Persistent("block directory offset overflow".into()))?;
    }
    if expected_offset != data_len {
        return Err(StoreError::Persistent(
            "block directory data length mismatch".into(),
        ));
    }
    Ok(())
}

fn encode_manifest(manifest: Manifest) -> [u8; MANIFEST_SIZE] {
    let mut bytes = [0u8; MANIFEST_SIZE];
    bytes[..8].copy_from_slice(MANIFEST_MAGIC);
    put_u32(&mut bytes, 8, MANIFEST_VERSION);
    put_u32(&mut bytes, 12, FORMAT_VERSION);
    bytes[16..48].copy_from_slice(&manifest.cache_key);
    put_u64(&mut bytes, 48, manifest.data_len);
    put_u64(&mut bytes, 56, manifest.index_len);
    put_u64(&mut bytes, 64, manifest.word_count);
    put_u64(&mut bytes, 72, manifest.created_unix_ns);
    put_u64(&mut bytes, 80, manifest.accessed_unix_ns);
    let checksum = crate::crc32c::block_checksum(&bytes, 88);
    put_u32(&mut bytes, 88, checksum);
    bytes
}

fn decode_manifest(bytes: &[u8]) -> StoreResult<Manifest> {
    if bytes.len() != MANIFEST_SIZE || &bytes[..8] != MANIFEST_MAGIC {
        return Err(StoreError::Persistent("invalid persistent manifest".into()));
    }
    if get_u32(bytes, 8)? != MANIFEST_VERSION || get_u32(bytes, 12)? != FORMAT_VERSION {
        return Err(StoreError::Persistent(
            "unsupported persistent manifest version".into(),
        ));
    }
    let expected_checksum = get_u32(bytes, 88)?;
    if crate::crc32c::block_checksum(bytes, 88) != expected_checksum {
        return Err(StoreError::Persistent(
            "persistent manifest checksum mismatch".into(),
        ));
    }
    let mut cache_key = [0u8; 32];
    cache_key.copy_from_slice(&bytes[16..48]);
    Ok(Manifest {
        cache_key,
        data_len: get_u64(bytes, 48)?,
        index_len: get_u64(bytes, 56)?,
        word_count: get_u64(bytes, 64)?,
        created_unix_ns: get_u64(bytes, 72)?,
        accessed_unix_ns: get_u64(bytes, 80)?,
    })
}

fn touch_manifest(path: &Path) -> StoreResult<()> {
    let mut manifest = decode_manifest(&fs::read(path)?)?;
    manifest.accessed_unix_ns = unix_ns();
    let tmp = path.with_extension("dwm.tmp");
    write_synced(&tmp, &encode_manifest(manifest))?;
    fs::rename(tmp, path)?;
    Ok(())
}

fn write_synced(path: &Path, bytes: &[u8]) -> StoreResult<()> {
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_data()?;
    Ok(())
}

fn remove_stale_temporaries(directory: &Path) {
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        if entry
            .file_name()
            .to_str()
            .is_some_and(|name| name.ends_with(".tmp"))
        {
            let _ = fs::remove_file(entry.path());
        }
    }
}

fn directory_size(directory: &Path) -> u64 {
    fs::read_dir(directory)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| entry.metadata().ok())
        .filter(|metadata| metadata.is_file())
        .map(|metadata| metadata.len())
        .sum()
}

fn remove_if_exists(path: &Path) -> StoreResult<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> StoreResult<()> {
    File::open(path)?.sync_all()?;
    Ok(())
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> StoreResult<()> {
    Ok(())
}

fn unix_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
        .min(u128::from(u64::MAX)) as u64
}

fn hex_key(key: &[u8; 32]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(64);
    for &byte in key {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

fn put_u32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn put_u64(bytes: &mut [u8], offset: usize, value: u64) {
    bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

fn put_optional_u64(bytes: &mut [u8], offset: usize, value: Option<u64>) {
    put_u64(bytes, offset, value.unwrap_or(u64::MAX));
}

fn get_u32(bytes: &[u8], offset: usize) -> StoreResult<u32> {
    let raw: [u8; 4] = bytes
        .get(offset..offset + 4)
        .ok_or_else(|| StoreError::Persistent("truncated persistent metadata".into()))?
        .try_into()
        .unwrap();
    Ok(u32::from_le_bytes(raw))
}

fn get_u64(bytes: &[u8], offset: usize) -> StoreResult<u64> {
    let raw: [u8; 8] = bytes
        .get(offset..offset + 8)
        .ok_or_else(|| StoreError::Persistent("truncated persistent metadata".into()))?
        .try_into()
        .unwrap();
    Ok(u64::from_le_bytes(raw))
}

fn get_optional_u64(bytes: &[u8], offset: usize) -> StoreResult<Option<u64>> {
    Ok(match get_u64(bytes, offset)? {
        u64::MAX => None,
        value => Some(value),
    })
}
