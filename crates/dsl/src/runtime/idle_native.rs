use std::time::Duration;

/// Prevents thread-driven native nodes from spinning while all inputs are
/// momentarily quiet.
pub(crate) fn idle_backoff() {
    std::thread::sleep(Duration::from_millis(2));
}
