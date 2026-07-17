use std::sync::{Arc, RwLock};

/// Thread-safe boolean activity timeline used by processing nodes to expose
/// when a generic sampling condition is active. It stores only level changes,
/// never individual clock/sample events.
#[derive(Debug, Clone, Default)]
pub struct SamplingActivity {
    transitions: Arc<RwLock<Vec<(u64, bool)>>>,
}

impl SamplingActivity {
    pub fn record_interval(&self, start_ns: u64, end_ns: u64) {
        if start_ns >= end_ns {
            return;
        }
        self.record_transitions([(start_ns, true), (end_ns, false)]);
    }

    pub fn record_intervals(&self, intervals: impl IntoIterator<Item = (u64, u64)>) {
        let transitions = intervals
            .into_iter()
            .filter(|(start, end)| start < end)
            .flat_map(|(start, end)| [(start, true), (end, false)]);
        self.record_transitions(transitions);
    }

    pub fn is_active_at(&self, time_ns: u64) -> bool {
        let transitions = self.transitions.read().unwrap();
        let index = transitions.partition_point(|(time, _)| *time <= time_ns);
        index
            .checked_sub(1)
            .and_then(|index| transitions.get(index))
            .is_some_and(|(_, active)| *active)
    }

    fn record_transitions(&self, transitions: impl IntoIterator<Item = (u64, bool)>) {
        let mut stored = self.transitions.write().unwrap();
        for (time, active) in transitions {
            if stored
                .last()
                .is_some_and(|(last_time, _)| *last_time > time)
            {
                let keep = stored.partition_point(|(stored_time, _)| *stored_time < time);
                stored.truncate(keep);
            }
            if stored
                .last()
                .is_some_and(|(last_time, _)| *last_time == time)
            {
                stored.pop();
            }
            if stored
                .last()
                .is_none_or(|(_, previous_active)| *previous_active != active)
            {
                stored.push((time, active));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adjacent_intervals_coalesce_without_a_boundary_glitch() {
        let activity = SamplingActivity::default();
        activity.record_interval(10, 20);
        activity.record_interval(20, 30);

        assert!(!activity.is_active_at(9));
        assert!(activity.is_active_at(10));
        assert!(activity.is_active_at(20));
        assert!(!activity.is_active_at(30));
        assert_eq!(
            &*activity.transitions.read().unwrap(),
            &[(10, true), (30, false)]
        );
    }

    #[test]
    fn separated_intervals_leave_the_gap_inactive() {
        let activity = SamplingActivity::default();
        activity.record_intervals([(10, 20), (25, 30)]);

        assert!(activity.is_active_at(19));
        assert!(!activity.is_active_at(20));
        assert!(!activity.is_active_at(24));
        assert!(activity.is_active_at(25));
    }

    #[test]
    fn recording_from_an_earlier_time_replaces_stale_activity() {
        let activity = SamplingActivity::default();
        activity.record_intervals([(10, 20), (30, 40)]);
        activity.record_interval(15, 25);

        assert!(activity.is_active_at(14));
        assert!(activity.is_active_at(20));
        assert!(!activity.is_active_at(25));
        assert!(!activity.is_active_at(35));
        assert_eq!(
            &*activity.transitions.read().unwrap(),
            &[(10, true), (25, false)]
        );
    }
}
