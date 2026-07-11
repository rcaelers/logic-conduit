//! Incremental multi-resolution summary index for derived lanes
//! (`docs/LOGIC_ANALYZER_VIEWER_DESIGN.md`), so the viewer never has to rescan
//! millions of raw entries per frame just to render (or measure) a
//! zoomed-out window.
//!
//! This mirrors the *idea* behind [`super::waveform_index`] — a
//! multi-resolution index on top of complete raw data, never a replacement
//! for it — but not its format: that index is boolean-per-sample, built in
//! one batch from a fully-known, randomly-readable capture, and lives in an
//! mmap'd file (desktop-only). Derived lanes arrive one entry at a time from
//! a running node, at irregular timestamps (not a fixed sample rate), and
//! must be indexable on wasm too (derived lanes are wasm's only viewer
//! content, since raw capture files are native-only). [`AppendOnlyMipmap`]
//! is a plain in-memory structure built for that: append one entry at a
//! time, query a coarse summary of any `[start_ns, end_ns]` window without
//! ever rescanning the raw entries.
//!
//! The raw entries themselves keep living in their own `Vec` next to this
//! index (`DerivedLaneData` in `nodes::sinks::viewer_sink`) — this index
//! only ever summarizes, it never stores the only copy of anything, which is
//! what "never drop data" requires.

/// One summary record: a run of raw entries collapsed into their time span,
/// count, and — only meaningful for a `Digital` lane, where `bool` is a
/// level rather than an opaque value — the first/last level in the run, for
/// flat-vs-activity-band rendering the same way
/// `CaptureWaveformSegment::Level`/`Activity` already work for raw channels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MipmapRecord {
    pub start_ns: u64,
    pub end_ns: u64,
    pub count: u32,
    pub level_hint: Option<(bool, bool)>,
}

/// How one derived-lane kind turns a raw entry into a leaf record, and folds
/// a run of same-tier records into the next coarser one. No `&self` — these
/// are pure, stateless rules per lane kind, dispatched at the type level via
/// [`AppendOnlyMipmap`]'s `F` parameter.
pub trait LaneFold<T> {
    fn leaf(entry: &T) -> MipmapRecord;
    /// `records` is always non-empty and time-sorted (it's a contiguous run
    /// from one tier of the mipmap, which is itself built only from
    /// time-sorted input).
    fn combine(records: &[MipmapRecord]) -> MipmapRecord;
}

/// How many records of one tier fold into one record of the tier above.
/// Matches `waveform_index`'s `LEVEL_POWER` (2^6) fan-out for familiarity;
/// unlike that index's fixed-sample-count groups, a group here always
/// covers exactly `FAN_OUT` raw entries but a *variable* time span, since
/// derived-lane entries arrive at irregular timestamps.
const FAN_OUT: usize = 64;
const CHUNK_SIZE: usize = 4_096;

/// Append-only multi-resolution index over one derived lane's raw entries.
/// `tiers[0]` holds one leaf record per raw entry (in the same order);
/// `tiers[k]` (k > 0) holds one record per `FAN_OUT` records of `tiers[k-1]`.
/// Every tier is retained forever — appending never coarsens or discards
/// anything already summarized, it only ever adds.
///
/// Building is amortized O(1) per append (standard carry-propagation, like
/// incrementing a base-`FAN_OUT` counter: most appends touch only `tiers[0]`,
/// a `1/FAN_OUT` fraction also touch `tiers[1]`, a `1/FAN_OUT^2` fraction
/// also touch `tiers[2]`, and so on). Querying a window is
/// O(log(entries)) — a `partition_point` binary search into whichever tier's
/// records best match the requested resolution.
#[derive(Debug, Clone)]
pub struct AppendOnlyMipmap<T, F> {
    tiers: Vec<Vec<MipmapRecord>>,
    len: usize,
    _fold: std::marker::PhantomData<fn(&T) -> F>,
}

impl<T, F: LaneFold<T>> AppendOnlyMipmap<T, F> {
    pub fn new() -> Self {
        Self {
            tiers: vec![Vec::new()],
            len: 0,
            _fold: std::marker::PhantomData,
        }
    }

    /// Number of raw entries appended so far.
    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn push(&mut self, entry: &T) {
        self.push_record(0, F::leaf(entry));
        self.len += 1;
    }

    pub fn extend<'a>(&mut self, entries: impl IntoIterator<Item = &'a T>)
    where
        T: 'a,
    {
        for entry in entries {
            self.push(entry);
        }
    }

    fn push_record(&mut self, tier: usize, record: MipmapRecord) {
        if tier == self.tiers.len() {
            self.tiers.push(Vec::new());
        }
        self.tiers[tier].push(record);
        if self.tiers[tier].len().is_multiple_of(FAN_OUT) {
            let start = self.tiers[tier].len() - FAN_OUT;
            let combined = F::combine(&self.tiers[tier][start..]);
            self.push_record(tier + 1, combined);
        }
    }

    /// Records covering `[start_ns, end_ns]`, from the coarsest tier whose
    /// windowed record count still fits comfortably within a rendering
    /// budget derived from `target_points` — the same
    /// "coarsest-resolution-that-still-fits" idea `waveform_index` uses, but
    /// chosen by actual record count in the window rather than a
    /// precomputed span-per-record, since records here don't have a fixed
    /// time span. Falls back to the finest (raw) tier if even that
    /// overflows the budget — better a busy render than a wrong one.
    ///
    /// A chosen tier only covers raw entries up to its last *complete*
    /// fan-out group — appends since then haven't been folded up into it
    /// yet. Those are exactly the most recently appended entries, i.e.
    /// exactly what a live view is usually looking at, so
    /// [`Self::append_uncovered_tail`] always folds them in as one extra
    /// record rather than letting a coarse, zoomed-out view of a live lane
    /// silently miss whatever just arrived.
    pub fn sampled_window(
        &self,
        start_ns: u64,
        end_ns: u64,
        target_points: usize,
    ) -> Vec<MipmapRecord> {
        let budget = target_points.max(1).saturating_mul(4);
        for tier_index in (0..self.tiers.len()).rev() {
            let tier = &self.tiers[tier_index];
            let (first, last) = Self::window_range(tier, start_ns, end_ns);
            if last - first <= budget || tier_index == 0 {
                // Padded by one record on each side (when available) so a
                // caller measuring a pulse/period at the window's edge has
                // the real neighboring record for context, the same reason
                // `logic_analyzer_viewer::channel::windowed_range` pads a
                // raw-Vec window by one entry.
                let first = first.saturating_sub(1);
                let last = (last + 1).min(tier.len());
                let mut result = tier[first..last].to_vec();
                self.append_uncovered_tail(tier_index, start_ns, end_ns, &mut result);
                return result;
            }
        }
        Vec::new()
    }

    /// `tiers[tier_index]` only ever covers raw entries
    /// `[0, tiers[tier_index].len() * FAN_OUT^tier_index)` — the exact
    /// invariant of the carry-propagation append (each tier's length is
    /// `self.len` repeatedly floor-divided by `FAN_OUT`, which is the same
    /// as one floor division by `FAN_OUT^tier_index`). Anything appended
    /// past that boundary hasn't completed a group at this tier yet; if it
    /// falls inside the query window, fold it (from the raw leaf tier, its
    /// only home so far) into one extra summary record.
    fn append_uncovered_tail(
        &self,
        tier_index: usize,
        start_ns: u64,
        end_ns: u64,
        out: &mut Vec<MipmapRecord>,
    ) {
        if tier_index == 0 {
            return; // tier 0 IS the raw leaf tier; nothing finer to miss.
        }
        let group_size = FAN_OUT.pow(tier_index as u32);
        let covered = self.tiers[tier_index].len() * group_size;
        if covered >= self.len {
            return;
        }
        let raw = &self.tiers[0][covered..];
        let (first, last) = Self::window_range(raw, start_ns, end_ns);
        if last > first {
            out.push(F::combine(&raw[first..last]));
        }
    }

    fn window_range(tier: &[MipmapRecord], start_ns: u64, end_ns: u64) -> (usize, usize) {
        let first = tier.partition_point(|record| record.end_ns < start_ns);
        let last = first + tier[first..].partition_point(|record| record.start_ns <= end_ns);
        (first, last)
    }
}

impl<T, F: LaneFold<T>> Default for AppendOnlyMipmap<T, F> {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Copy)]
struct SummaryRecordFold;

impl LaneFold<MipmapRecord> for SummaryRecordFold {
    fn leaf(entry: &MipmapRecord) -> MipmapRecord {
        *entry
    }

    fn combine(records: &[MipmapRecord]) -> MipmapRecord {
        let first = records[0];
        let last = records[records.len() - 1];
        MipmapRecord {
            start_ns: first.start_ns,
            end_ns: records.iter().map(|record| record.end_ns).max().unwrap(),
            count: records.iter().map(|record| record.count).sum(),
            level_hint: match (first.level_hint, last.level_hint) {
                (Some((first, _)), Some((_, last))) => Some((first, last)),
                _ => None,
            },
        }
    }
}

/// Memory-bounded summary for raw data that already lives in a separately
/// searchable vector. The active chunk keeps leaf records so recent entries
/// remain exact. Once full, it folds to one immutable record and reuses the
/// same allocation; a small mipmap over completed chunks keeps wide queries
/// bounded without retaining a second leaf record for every raw entry.
#[derive(Debug, Clone)]
pub struct ChunkedMipmap<T, F> {
    completed: AppendOnlyMipmap<MipmapRecord, SummaryRecordFold>,
    active: Vec<MipmapRecord>,
    len: usize,
    _fold: std::marker::PhantomData<fn(&T) -> F>,
}

impl<T, F: LaneFold<T>> ChunkedMipmap<T, F> {
    pub fn new() -> Self {
        Self {
            completed: AppendOnlyMipmap::new(),
            active: Vec::with_capacity(CHUNK_SIZE),
            len: 0,
            _fold: std::marker::PhantomData,
        }
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn push(&mut self, entry: &T) {
        self.active.push(F::leaf(entry));
        self.len += 1;
        if self.active.len() == CHUNK_SIZE {
            let summary = F::combine(&self.active);
            self.completed.push(&summary);
            self.active.clear();
        }
    }

    pub fn extend<'a>(&mut self, entries: impl IntoIterator<Item = &'a T>)
    where
        T: 'a,
    {
        for entry in entries {
            self.push(entry);
        }
    }

    pub fn sampled_window(
        &self,
        start_ns: u64,
        end_ns: u64,
        target_points: usize,
    ) -> Vec<MipmapRecord> {
        let mut result = self
            .completed
            .sampled_window(start_ns, end_ns, target_points);
        let first = self
            .active
            .partition_point(|record| record.end_ns < start_ns);
        let last = first + self.active[first..].partition_point(|record| record.start_ns <= end_ns);
        if self.active.is_empty() {
            return result;
        }

        let first = first.saturating_sub(1);
        let last = (last + 1).min(self.active.len());
        let active = &self.active[first..last];
        let budget = target_points.max(1).saturating_mul(4);
        if result.len().saturating_add(active.len()) <= budget {
            result.extend_from_slice(active);
        } else {
            result.push(F::combine(active));
        }
        result
    }
}

impl<T, F: LaneFold<T>> Default for ChunkedMipmap<T, F> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone, Copy)]
    struct Event(u64);

    struct EventFold;
    impl LaneFold<Event> for EventFold {
        fn leaf(entry: &Event) -> MipmapRecord {
            MipmapRecord {
                start_ns: entry.0,
                end_ns: entry.0,
                count: 1,
                level_hint: None,
            }
        }
        fn combine(records: &[MipmapRecord]) -> MipmapRecord {
            MipmapRecord {
                start_ns: records[0].start_ns,
                end_ns: records[records.len() - 1].end_ns,
                count: records.iter().map(|record| record.count).sum(),
                level_hint: None,
            }
        }
    }

    #[test]
    fn empty_mipmap_has_no_records_anywhere() {
        let mipmap = AppendOnlyMipmap::<Event, EventFold>::new();
        assert!(mipmap.is_empty());
        assert_eq!(mipmap.sampled_window(0, u64::MAX, 100), &[]);
    }

    #[test]
    fn len_tracks_every_push() {
        let mut mipmap = AppendOnlyMipmap::<Event, EventFold>::new();
        for i in 0..200u64 {
            mipmap.push(&Event(i * 10));
        }
        assert_eq!(mipmap.len(), 200);
    }

    #[test]
    fn tier_0_is_one_record_per_entry_when_queried_at_full_resolution() {
        let mut mipmap = AppendOnlyMipmap::<Event, EventFold>::new();
        for i in 0..10u64 {
            mipmap.push(&Event(i));
        }
        // Asking for as many points as entries forces the finest tier.
        let window = mipmap.sampled_window(0, 9, 10);
        assert_eq!(window.len(), 10);
        for (i, record) in window.iter().enumerate() {
            assert_eq!(record.start_ns, i as u64);
            assert_eq!(record.end_ns, i as u64);
            assert_eq!(record.count, 1);
        }
    }

    #[test]
    fn a_full_fan_out_group_folds_into_one_tier_1_record() {
        let mut mipmap = AppendOnlyMipmap::<Event, EventFold>::new();
        for i in 0..FAN_OUT as u64 {
            mipmap.push(&Event(i));
        }
        // Asking for very few points should pick a coarse tier — with
        // exactly one fan-out group pushed, tier 1 has exactly one record
        // spanning the whole run.
        let window = mipmap.sampled_window(0, FAN_OUT as u64 - 1, 1);
        assert_eq!(window.len(), 1);
        assert_eq!(window[0].start_ns, 0);
        assert_eq!(window[0].end_ns, FAN_OUT as u64 - 1);
        assert_eq!(window[0].count, FAN_OUT as u32);
    }

    #[test]
    fn query_window_excludes_records_far_outside_the_range() {
        let mut mipmap = AppendOnlyMipmap::<Event, EventFold>::new();
        for i in 0..20u64 {
            mipmap.push(&Event(i * 100));
        }
        let window = mipmap.sampled_window(500, 1000, 20);
        // Entries at 500, 600, ..., 1000 inclusive, plus a one-record pad on
        // each side (400, 1100) for boundary-adjacent measurement context —
        // but nothing beyond that.
        assert_eq!(window.len(), 8);
        assert_eq!(window.first().unwrap().start_ns, 400);
        assert_eq!(window.last().unwrap().start_ns, 1100);
    }

    #[test]
    fn a_coarse_query_still_includes_the_most_recently_appended_entries() {
        // Regression test for the "uncovered tail" gap: a tier only covers
        // raw entries up to its last *complete* fan-out group, so the most
        // recently appended entries — exactly what a live, zoomed-out view
        // usually wants to see — must still show up via the tail fold, not
        // silently vanish until enough more arrive to complete a group.
        let mut mipmap = AppendOnlyMipmap::<Event, EventFold>::new();
        let total = FAN_OUT as u64 * FAN_OUT as u64 + 5; // one full tier-1 group, plus a tail
        for i in 0..total {
            mipmap.push(&Event(i));
        }
        let window = mipmap.sampled_window(0, total - 1, 1);
        let total_count: u64 = window.iter().map(|record| record.count as u64).sum();
        assert_eq!(
            total_count, total,
            "every entry must be represented, including the not-yet-grouped tail"
        );
        assert_eq!(
            window.last().unwrap().end_ns,
            total - 1,
            "the tail record must reach all the way to the last appended entry"
        );
    }

    #[test]
    fn many_thousands_of_entries_stay_cheap_to_query() {
        // Not a perf assertion (that would be flaky in CI) — a correctness
        // check that querying a huge lane at a coarse resolution actually
        // returns a small, bounded number of records, proving the tiers are
        // doing their job rather than silently falling back to raw data.
        let mut mipmap = AppendOnlyMipmap::<Event, EventFold>::new();
        let total = 200_000u64;
        for i in 0..total {
            mipmap.push(&Event(i));
        }
        let window = mipmap.sampled_window(0, total - 1, 1_000);
        assert!(
            window.len() <= 1_000 * 4,
            "expected a coarse tier, got {} records for a 4000-point budget",
            window.len()
        );
        // And the coarse window still accounts for every entry.
        let total_count: u64 = window.iter().map(|record| record.count as u64).sum();
        assert_eq!(total_count, total);
    }

    #[test]
    fn combine_is_never_called_on_an_empty_slice() {
        // Regression guard: `push_record` must only ever call `F::combine`
        // on a freshly completed fan-out group, never an empty one.
        struct PanicsOnEmpty;
        impl LaneFold<Event> for PanicsOnEmpty {
            fn leaf(entry: &Event) -> MipmapRecord {
                EventFold::leaf(entry)
            }
            fn combine(records: &[MipmapRecord]) -> MipmapRecord {
                assert!(!records.is_empty(), "combine called on an empty slice");
                EventFold::combine(records)
            }
        }
        let mut mipmap = AppendOnlyMipmap::<Event, PanicsOnEmpty>::new();
        for i in 0..(FAN_OUT as u64 * FAN_OUT as u64 + 5) {
            mipmap.push(&Event(i));
        }
    }

    #[test]
    fn chunked_mipmap_folds_completed_chunks_and_reuses_active_storage() {
        let mut mipmap = ChunkedMipmap::<Event, EventFold>::new();
        let total = CHUNK_SIZE + 5;
        for index in 0..total as u64 {
            mipmap.push(&Event(index));
        }

        assert_eq!(mipmap.len(), total);
        assert_eq!(mipmap.completed.len(), 1);
        assert_eq!(mipmap.active.len(), 5);
        assert_eq!(mipmap.active.capacity(), CHUNK_SIZE);

        let window = mipmap.sampled_window(0, total as u64 - 1, 1);
        assert_eq!(
            window
                .iter()
                .map(|record| record.count as usize)
                .sum::<usize>(),
            total
        );
        assert_eq!(window.first().unwrap().start_ns, 0);
        assert_eq!(window.last().unwrap().end_ns, total as u64 - 1);
    }
}
