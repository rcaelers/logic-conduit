use std::path::Path;

use signal_processing::CaptureDataSource;

pub(crate) fn capture_cache_identity<S>(path: &Path, source: &S) -> [u8; 32]
where
    S: CaptureDataSource,
{
    let metadata = source.metadata();
    let file_metadata = std::fs::metadata(path).ok();
    let mut hasher = blake3::Hasher::new();
    hasher.update(path.to_string_lossy().as_bytes());
    hasher.update(&source.fingerprint().revision.to_le_bytes());
    hasher.update(&metadata.samplerate_hz.to_bits().to_le_bytes());
    hasher.update(&metadata.total_samples.to_le_bytes());
    hasher.update(&(metadata.total_probes as u64).to_le_bytes());
    if let Some(file_metadata) = file_metadata {
        hasher.update(&file_metadata.len().to_le_bytes());
        if let Ok(modified) = file_metadata.modified()
            && let Ok(modified) = modified.duration_since(std::time::UNIX_EPOCH)
        {
            hasher.update(&modified.as_nanos().to_le_bytes());
        }
    }
    for name in &metadata.probe_names {
        hasher.update(name.as_bytes());
    }
    *hasher.finalize().as_bytes()
}
