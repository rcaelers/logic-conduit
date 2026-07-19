mod builder;
mod exact;
mod growing;
mod query;
mod reader;
mod storage;
mod types;

pub use exact::exact_window_sample_limit;
pub use growing::{NativeGrowingCaptureIndex, NativeGrowingCaptureIndexWorker};
pub use reader::IndexSampler;
pub use types::CaptureIndexProgress;

/// Selects the finest available summary resolution that does not exceed the
/// requested samples-per-point scale. Both finite and growing indexes use this
/// policy so changing capture state cannot change zoom semantics.
fn select_summary_resolution(
    window_samples: u64,
    target_points: usize,
    available_resolutions: impl IntoIterator<Item = u64>,
) -> Option<u64> {
    let desired = window_samples
        .div_ceil(target_points.max(1) as u64)
        .max(1);
    let mut available = available_resolutions.into_iter().collect::<Vec<_>>();
    available.sort_unstable();
    available.dedup();
    let mut selected = None;
    for resolution in available {
        if selected.is_none() || resolution <= desired {
            selected = Some(resolution);
        }
        if resolution > desired {
            break;
        }
    }
    selected
}

#[cfg(test)]
mod resolution_tests {
    use super::select_summary_resolution;

    #[test]
    fn selects_same_mipmap_scale_for_a_window_and_point_budget() {
        let resolutions = [64, 4_096, 262_144, 16_777_216];
        assert_eq!(
            select_summary_resolution(100_000, 1_000, resolutions),
            Some(64)
        );
        assert_eq!(
            select_summary_resolution(1_048_576, 100, resolutions),
            Some(4_096)
        );
        assert_eq!(
            select_summary_resolution(100_000_000, 100, resolutions),
            Some(262_144)
        );
    }
}
