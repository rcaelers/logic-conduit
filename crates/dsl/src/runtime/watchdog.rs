//! Channel operation watchdog for detecting deadlocks
//!
//! Low-overhead monitoring using atomic timestamps instead of locks.
//! Each receiver/sender stores its operation start time in an atomic variable,
//! and the watchdog periodically scans these timestamps to detect blocking.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;
use tracing::{info, warn};
// `std::time::SystemTime::now()` panics on `wasm32-unknown-unknown` (no clock
// syscall); `web_time` provides the same API backed by `Date.now()` in the
// browser and transparently re-exports `std::time` elsewhere.
use web_time::{SystemTime, UNIX_EPOCH};

/// Timestamp in milliseconds since UNIX_EPOCH
#[inline(always)]
fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::from_secs(0))
        .as_millis() as u64
}

/// Shared state for a single port's operation tracking
struct PortState {
    /// Timestamp (ms since epoch) when current operation started, or 0 if idle
    last_op_start: AtomicU64,
    /// Track if we've already warned about this port being blocked
    has_warned: AtomicBool,
    node_name: String,
    port_name: String,
    operation: String, // "recv", "send", etc.
}

/// Handle to a port's watchdog state (held by receiver/sender wrappers)
#[derive(Clone)]
pub struct WatchdogHandle {
    state: Arc<PortState>,
}

impl WatchdogHandle {
    /// Mark the start of a blocking operation (stores current timestamp)
    #[inline(always)]
    pub fn start_operation(&self) {
        self.state
            .last_op_start
            .store(now_millis(), Ordering::Relaxed);
        // Reset warning flag for new operation
        self.state.has_warned.store(false, Ordering::Relaxed);
    }

    /// Mark the end of a blocking operation (clears timestamp to 0)
    #[inline(always)]
    pub fn finish_operation(&self) {
        // Check if we warned about this operation being blocked
        if self.state.has_warned.load(Ordering::Relaxed) {
            info!(
                "✅ UNBLOCKED: [{}] {} on port '{}'",
                self.state.node_name, self.state.operation, self.state.port_name
            );
            self.state.has_warned.store(false, Ordering::Relaxed);
        }
        self.state.last_op_start.store(0, Ordering::Relaxed);
    }
}

/// Shared watchdog state
#[derive(Clone)]
pub struct Watchdog {
    ports: Arc<Mutex<Vec<Weak<PortState>>>>,
    enabled: Arc<Mutex<bool>>,
}

impl Watchdog {
    /// Create a new watchdog
    pub fn new() -> Self {
        Self {
            ports: Arc::new(Mutex::new(Vec::new())),
            enabled: Arc::new(Mutex::new(true)),
        }
    }

    /// Register a new port for monitoring
    pub fn register_port(
        &self,
        node_name: &str,
        operation: &str,
        port_name: &str,
    ) -> WatchdogHandle {
        let state = Arc::new(PortState {
            last_op_start: AtomicU64::new(0),
            has_warned: AtomicBool::new(false),
            node_name: node_name.to_string(),
            port_name: port_name.to_string(),
            operation: operation.to_string(),
        });

        self.ports.lock().unwrap().push(Arc::downgrade(&state));

        WatchdogHandle { state }
    }

    /// Check for blocked operations (>5 seconds)
    pub fn check_for_blocked(&self) {
        let now = now_millis();
        let threshold_ms = 5000; // 5 seconds

        let mut ports = self.ports.lock().unwrap();

        // Remove dead weak references and check live ones
        ports.retain(|weak| {
            if let Some(state) = weak.upgrade() {
                let start = state.last_op_start.load(Ordering::Relaxed);
                if start > 0 {
                    let duration_ms = now.saturating_sub(start);
                    if duration_ms > threshold_ms {
                        // Only warn once per blocking operation
                        if !state.has_warned.swap(true, Ordering::Relaxed) {
                            warn!(
                                "⚠️  BLOCKED: [{}] {} on port '{}' for {:.1}s",
                                state.node_name,
                                state.operation,
                                state.port_name,
                                duration_ms as f64 / 1000.0
                            );
                        }
                    }
                }
                true // Keep this weak reference
            } else {
                false // Remove dead weak reference
            }
        });
    }

    /// Start the watchdog monitoring thread
    pub fn start_monitoring_thread(&self) -> std::thread::JoinHandle<()> {
        let watchdog = self.clone();
        std::thread::spawn(move || {
            loop {
                std::thread::sleep(Duration::from_secs(1));

                if !*watchdog.enabled.lock().unwrap() {
                    break;
                }

                watchdog.check_for_blocked();
            }
        })
    }

    /// Stop the watchdog monitoring thread
    pub fn stop(&self) {
        *self.enabled.lock().unwrap() = false;
    }
}

impl Default for Watchdog {
    fn default() -> Self {
        Self::new()
    }
}

/// RAII guard for tracking an operation - near-zero cost (just atomic stores)
///
/// Uses a reference to avoid Arc cloning overhead (no reference count manipulation).
pub struct OperationGuard<'a> {
    handle: &'a WatchdogHandle,
}

impl<'a> OperationGuard<'a> {
    #[inline(always)]
    pub fn new(handle: &'a WatchdogHandle) -> Self {
        handle.start_operation();
        Self { handle }
    }
}

impl Drop for OperationGuard<'_> {
    #[inline(always)]
    fn drop(&mut self) {
        self.handle.finish_operation();
    }
}
