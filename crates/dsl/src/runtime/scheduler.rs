//! Thread-per-node scheduler for streaming graphs
//!
//! Spawns a dedicated thread for each node and manages their lifecycle.
//!
//! ## Threading Models
//!
//! The scheduler supports two threading models:
//!
//! 1. **Regular nodes**: Scheduler calls `work()` repeatedly in a loop. The node processes
//!    one batch of items per call and returns the count. The scheduler thread does the looping.
//!
//! 2. **Self-threading nodes**: Node manages its own worker threads internally. Scheduler calls
//!    `work()` once to start the node, then waits for `should_stop()` to signal completion.
//!    The node returns `is_self_threading() = true` to indicate this pattern.
//!
//! Example self-threading node: `DslFileSource` spawns per-channel reader threads internally.

use super::node::ProcessNode;
use super::ports::{InputPort, OutputPort};
use super::watchdog::Watchdog;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver as StdReceiver, Sender as StdSender, channel};
use std::thread::{self, JoinHandle};
use tracing::{debug, error, info};

/// Runtime scheduler that executes a streaming graph
pub struct Scheduler {
    threads: Vec<(String, JoinHandle<()>)>,
    stop_signal: Arc<AtomicBool>,
    completion_tx: StdSender<String>,
    completion_rx: Option<StdReceiver<String>>,
    watchdog: Watchdog,
    watchdog_handle: JoinHandle<()>,
}

impl Scheduler {
    /// Create a new scheduler with watchdog monitoring
    pub fn new() -> Self {
        let (completion_tx, completion_rx) = channel();
        let watchdog = Watchdog::new();
        let watchdog_handle = watchdog.start_monitoring_thread();
        info!("Watchdog enabled - will report operations blocked >5 seconds");
        Self {
            threads: Vec::new(),
            stop_signal: Arc::new(AtomicBool::new(false)),
            completion_tx,
            completion_rx: Some(completion_rx),
            watchdog,
            watchdog_handle,
        }
    }

    /// Get a reference to the watchdog
    pub fn watchdog(&self) -> &Watchdog {
        &self.watchdog
    }

    /// Start a process node in its own thread
    /// Process nodes include sources (0 inputs), sinks (0 outputs), and transformers (N inputs, M outputs)
    pub fn start_process(
        &mut self,
        mut node: Box<dyn ProcessNode>,
        inputs: Vec<InputPort>,
        outputs: Vec<OutputPort>,
    ) {
        let stop_signal = Arc::clone(&self.stop_signal);
        let completion_tx = self.completion_tx.clone();
        let name = node.name().to_string();
        let thread_name = name.clone();

        debug!("Starting process node: {}", name);

        let handle = thread::spawn(move || {
            if node.is_self_threading() {
                // Self-threading node: call work() once to start internal threads
                if let Err(e) = node.work(&inputs, &outputs) {
                    error!(
                        "[{}] Failed to start self-threading node: {}",
                        thread_name, e
                    );
                } else {
                    // Wait for node to signal completion via should_stop() or stop_signal
                    loop {
                        if stop_signal.load(Ordering::Relaxed) {
                            info!(
                                "[{}] Stop signal received, shutting down self-threading node",
                                thread_name
                            );
                            break;
                        }
                        if node.should_stop() {
                            info!("[{}] Self-threading node completed", thread_name);
                            break;
                        }
                        thread::sleep(std::time::Duration::from_millis(100));
                    }
                }

                // Drop outputs/inputs/node to trigger shutdown
                drop(outputs);
                drop(inputs);
                drop(node);
            } else {
                // Regular node: call work() repeatedly
                let mut items_produced = 0usize;

                loop {
                    if stop_signal.load(Ordering::Relaxed) || node.should_stop() {
                        break;
                    }

                    match node.work(&inputs, &outputs) {
                        Ok(n) => {
                            items_produced += n;
                        }
                        Err(e) => {
                            error!("[{}] Work error: {}", thread_name, e);
                            break;
                        }
                    }
                }

                info!(
                    "[{}] Shutdown. Produced {} items.",
                    thread_name, items_produced
                );

                // Drop outputs/inputs/node to close channels
                drop(outputs);
                drop(inputs);
                drop(node);
            }

            // Notify scheduler that this thread is about to complete
            let _ = completion_tx.send(thread_name.clone());
        });

        self.threads.push((name, handle));
    }

    /// Signal all nodes to stop
    pub fn stop(&self) {
        self.stop_signal.store(true, Ordering::Relaxed);
    }

    /// Wait for all node threads to complete
    /// Uses a completion notification channel to join threads as they finish
    pub fn wait(mut self) {
        let completion_rx = self
            .completion_rx
            .take()
            .expect("completion_rx already taken");

        // Drop the main completion sender so the channel closes when all threads complete
        drop(self.completion_tx);

        let total_threads = self.threads.len();
        let mut completed = 0;

        info!("Waiting for {} threads to complete...", total_threads);

        // Convert to HashMap for O(1) lookup by name
        let mut threads_by_name: HashMap<String, JoinHandle<()>> =
            self.threads.into_iter().collect();

        // Block on completion notifications - no busy-waiting!
        while completed < total_threads {
            match completion_rx.recv() {
                Ok(thread_name) => {
                    completed += 1;
                    if let Some(handle) = threads_by_name.remove(&thread_name) {
                        match handle.join() {
                            Ok(_) => info!(
                                "[{}] Thread completed ({}/{})",
                                thread_name, completed, total_threads
                            ),
                            Err(e) => error!(
                                "[{}] Thread panicked ({}/{}): {:?}",
                                thread_name, completed, total_threads, e
                            ),
                        }
                    }
                }
                Err(_) => {
                    // Channel closed - all thread senders dropped
                    break;
                }
            }
        }

        info!("All {} threads completed", total_threads);

        // Stop watchdog
        self.watchdog.stop();
        let _ = self.watchdog_handle.join();
    }

    /// Get the number of running threads
    pub fn num_threads(&self) -> usize {
        self.threads.len()
    }

    /// Get the names of all running threads
    pub fn thread_names(&self) -> Vec<String> {
        self.threads.iter().map(|(name, _)| name.clone()).collect()
    }
}

impl Default for Scheduler {
    fn default() -> Self {
        Self::new()
    }
}

// /// Helper to create channels for a connection
// pub fn create_channel<T: Send>(buffer_size: usize) -> (Sender<T>, Receiver<T>) {
//     bounded(buffer_size)
// }

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::node::{ProcessNode, WorkError, WorkResult};
    use crate::runtime::sender::ChannelMessage;
    use crossbeam_channel::bounded;
    use std::sync::Mutex;
    use std::time::Duration;

    struct TestSource {
        count: usize,
        max: usize,
    }

    impl ProcessNode for TestSource {
        fn name(&self) -> &str {
            "test_source"
        }

        fn should_stop(&self) -> bool {
            self.count >= self.max
        }

        fn num_inputs(&self) -> usize {
            0 // Source
        }

        fn num_outputs(&self) -> usize {
            1
        }

        fn work(&mut self, _inputs: &[InputPort], outputs: &[OutputPort]) -> WorkResult<usize> {
            let output = outputs[0]
                .get::<u32>()
                .ok_or_else(|| WorkError::NodeError("Missing output channel".to_string()))?;

            if self.count < self.max {
                output.send(self.count as u32)?;
                self.count += 1;
                Ok(1)
            } else {
                Ok(0)
            }
        }
    }

    struct TestSink {
        received: Arc<Mutex<Vec<u32>>>,
    }

    impl ProcessNode for TestSink {
        fn name(&self) -> &str {
            "test_sink"
        }

        fn num_inputs(&self) -> usize {
            1
        }

        fn num_outputs(&self) -> usize {
            0 // Sink
        }

        fn work(&mut self, inputs: &[InputPort], _outputs: &[OutputPort]) -> WorkResult<usize> {
            let mut input_buffer = std::collections::VecDeque::new();
            let mut input = inputs[0]
                .get::<u32>(&mut input_buffer)
                .ok_or_else(|| WorkError::NodeError("Missing input channel".to_string()))?;

            match input.recv_timeout(Duration::from_millis(100)) {
                Ok(value) => {
                    self.received.lock().unwrap().push(value);
                    Ok(1)
                }
                Err(_) => {
                    tracing::debug!("[TestSink] recv_timeout error, returning Shutdown");
                    Err(WorkError::Shutdown)
                }
            }
        }
    }

    #[test]
    fn test_scheduler_basic() {
        let mut scheduler = Scheduler::new();

        let (tx, rx) = bounded::<ChannelMessage<u32>>(10);

        let source = TestSource { count: 0, max: 5 };
        let received = Arc::new(Mutex::new(Vec::new()));
        let sink = TestSink {
            received: Arc::clone(&received),
        };

        // Create test watchdog
        let watchdog = crate::runtime::Watchdog::new();

        // Source has 0 inputs, 1 output
        let source_outputs = vec![OutputPort::new_with_watchdog(
            crate::runtime::Sender::new(vec![tx]),
            &watchdog,
            "test_source",
            "output",
        )];
        scheduler.start_process(Box::new(source), vec![], source_outputs);

        // Sink has 1 input, 0 outputs
        let sink_inputs = vec![InputPort::new_with_watchdog(
            rx,
            &watchdog,
            "test_sink",
            "input",
        )];
        scheduler.start_process(Box::new(sink), sink_inputs, vec![]);

        thread::sleep(Duration::from_millis(200));

        let values = received.lock().unwrap();
        assert_eq!(*values, vec![0, 1, 2, 3, 4]);
    }

    // Self-threading test node that runs until stopped
    struct SelfThreadingTestNode {
        stop: Arc<AtomicBool>,
        completed: Arc<AtomicBool>,
    }

    impl ProcessNode for SelfThreadingTestNode {
        fn name(&self) -> &str {
            "self_threading_test"
        }

        fn is_self_threading(&self) -> bool {
            true
        }

        fn should_stop(&self) -> bool {
            self.completed.load(Ordering::Relaxed)
        }

        fn num_inputs(&self) -> usize {
            0
        }

        fn num_outputs(&self) -> usize {
            0
        }

        fn work(&mut self, _inputs: &[InputPort], _outputs: &[OutputPort]) -> WorkResult<usize> {
            let stop = Arc::clone(&self.stop);
            let completed = Arc::clone(&self.completed);

            // Spawn internal worker thread
            thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    thread::sleep(Duration::from_millis(10));
                }
                completed.store(true, Ordering::Relaxed);
            });

            Ok(0)
        }
    }

    impl Drop for SelfThreadingTestNode {
        fn drop(&mut self) {
            // Signal thread to stop
            self.stop.store(true, Ordering::Relaxed);
            // Wait for completion (with timeout to avoid hanging test)
            for _ in 0..100 {
                if self.completed.load(Ordering::Relaxed) {
                    break;
                }
                thread::sleep(Duration::from_millis(10));
            }
        }
    }

    #[test]
    fn test_scheduler_stop_signal_self_threading() {
        let mut scheduler = Scheduler::new();

        let stop = Arc::new(AtomicBool::new(false));
        let completed = Arc::new(AtomicBool::new(false));

        let node = SelfThreadingTestNode {
            stop: Arc::clone(&stop),
            completed: Arc::clone(&completed),
        };

        scheduler.start_process(Box::new(node), vec![], vec![]);

        // Wait a bit to ensure thread starts
        thread::sleep(Duration::from_millis(50));

        // Signal stop
        scheduler.stop();

        // Wait for completion (this should happen quickly)
        let start = std::time::Instant::now();
        scheduler.wait();
        let elapsed = start.elapsed();

        // Should complete within a reasonable time (not hang forever)
        assert!(
            elapsed < Duration::from_secs(2),
            "Scheduler took too long to stop: {:?}",
            elapsed
        );

        // Verify the node's thread was stopped
        assert!(
            completed.load(Ordering::Relaxed),
            "Self-threading node did not complete"
        );
    }
}
