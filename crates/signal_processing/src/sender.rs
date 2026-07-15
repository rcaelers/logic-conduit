//! Broadcast sender with watchdog monitoring for deadlock detection.
//!
//! Two broadcast flavors coexist (`docs/PIPELINE_DESIGN.md`):
//!
//! - **Static destinations** (`Sender::new`): the offline `Pipeline::build`
//!   path. Endpoints move into node threads; teardown is the crossbeam
//!   drop-cascade.
//! - **Shared subscriber list** (`SharedSenders` + `Sender::from_shared`):
//!   the live path. The list is owned by the `PipelineManager`, so a node
//!   thread exiting does *not* close downstream channels, subscribers can be
//!   added/removed mid-stream, and level channels prime late joiners with
//!   the last value (the level-stream contract extended to join time).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crossbeam_channel::{SendError, Sender as CrossbeamSender, TrySendError, bounded};

use super::watchdog::{OperationGuard, WatchdogHandle};

/// Channel message wrapper for end-of-stream signaling
///
/// Wraps data flowing through channels so sources can explicitly signal
/// when no more data will be sent. This is essential for self-threading
/// nodes (like `DslFileSource`) where `split_senders()` creates cloned
/// channel handles — dropping a clone doesn't close the channel because
/// the original `Sender` in `OutputPort` still holds its handles.
///
/// Nodes never see this enum directly — `Sender::send()` wraps values
/// in `Sample(T)` and `Receiver::recv()` unwraps them transparently.
#[derive(Clone, Debug)]
pub enum ChannelMessage<T> {
    /// A data sample
    Sample(T),
    /// Multiple ordered samples transported in one channel operation.
    Batch(Vec<T>),
    /// End-of-stream marker — no more data will be sent
    EndOfStream,
}

/// What a full subscriber buffer does to the producer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverflowPolicy {
    /// Block until the subscriber has room — lossless flow control
    /// (offline/file pipelines; anything feeding a persisting sink).
    Block,
    /// Never block: keep the newest value pending and drop what it
    /// supersedes. For level channels this coalesces to the latest value
    /// (which is exactly what a display wants); for event channels it drops
    /// bursts. Live viewer taps only.
    Lossy,
    /// Block up to the deadline, then unsubscribe the laggard and report it
    /// (via [`SharedSenders::take_disconnected`]) so the editor can badge
    /// the branch instead of silently corrupting a live capture.
    Disconnect(Duration),
}

static NEXT_SUBSCRIPTION_ID: AtomicU64 = AtomicU64::new(1);

struct Subscriber<T> {
    id: u64,
    tx: CrossbeamSender<ChannelMessage<T>>,
    policy: OverflowPolicy,
    label: Option<String>,
    /// `Lossy` only: newest value that did not fit; retried before the next
    /// send and overwritten by it when still unsendable.
    pending: Option<T>,
}

struct SharedSendersInner<T> {
    subscribers: Vec<Subscriber<T>>,
    /// Level channels remember the last sent value to prime late joiners.
    sticky: bool,
    last: Option<T>,
    /// Subscription ids dropped by [`OverflowPolicy::Disconnect`].
    disconnected: Vec<u64>,
    /// `close()` was called: late subscribers get an immediate EOS.
    closed: bool,
}

/// Supervisor-owned broadcast subscriber list. Cloning shares the list.
///
/// The lock is a `Mutex` rather than the `RwLock` sketched in the design:
/// each port has exactly one sending thread, so the lock is uncontended
/// except during rewires — and sends never hold it across a blocking
/// channel operation (see [`Sender::send`]).
pub struct SharedSenders<T> {
    inner: Arc<Mutex<SharedSendersInner<T>>>,
}

impl<T> Clone for SharedSenders<T> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<T: Clone + Send> SharedSenders<T> {
    /// `sticky` enables last-value priming — set it for level streams
    /// (`Sample`, `NumberSample`, `TextSample`), never for events.
    pub fn new(sticky: bool) -> Self {
        Self {
            inner: Arc::new(Mutex::new(SharedSendersInner {
                subscribers: Vec::new(),
                sticky,
                last: None,
                disconnected: Vec::new(),
                closed: false,
            })),
        }
    }

    /// Creates a bounded channel, adds its sender as a subscriber, and
    /// returns `(subscription id, receiver)`. Sticky lists immediately
    /// prime the new channel with the last value (original timestamp — the
    /// consumer treats it as its initial level). Subscribing to a closed
    /// list yields an immediate end-of-stream.
    pub fn subscribe(
        &self,
        buffer: usize,
        policy: OverflowPolicy,
    ) -> (u64, crossbeam_channel::Receiver<ChannelMessage<T>>) {
        self.subscribe_with_label(buffer, policy, None)
    }

    pub fn subscribe_with_label(
        &self,
        buffer: usize,
        policy: OverflowPolicy,
        label: Option<String>,
    ) -> (u64, crossbeam_channel::Receiver<ChannelMessage<T>>) {
        let (tx, rx) = bounded(buffer.max(1));
        let id = NEXT_SUBSCRIPTION_ID.fetch_add(1, Ordering::Relaxed);
        let mut inner = self.inner.lock().unwrap();
        if inner.closed {
            let _ = tx.send(ChannelMessage::EndOfStream);
            return (id, rx);
        }
        if inner.sticky
            && let Some(last) = inner.last.clone()
        {
            // Buffer is fresh and at least 1 deep; this cannot block.
            let _ = tx.try_send(ChannelMessage::Sample(last));
        }
        inner.subscribers.push(Subscriber {
            id,
            tx,
            policy,
            label,
            pending: None,
        });
        (id, rx)
    }

    /// Removes a subscription; dropping its sender disconnects the
    /// subscriber's channel, which downstream reads as end-of-stream.
    pub fn unsubscribe(&self, id: u64) {
        let mut inner = self.inner.lock().unwrap();
        inner.subscribers.retain(|subscriber| subscriber.id != id);
    }

    /// Sends end-of-stream to every subscriber and marks the list closed.
    /// This is the supervisor-driven equivalent of the offline drop-cascade.
    pub fn close(&self) {
        let mut inner = self.inner.lock().unwrap();
        inner.closed = true;
        for subscriber in inner.subscribers.drain(..) {
            let _ = subscriber.tx.send(ChannelMessage::EndOfStream);
        }
    }

    /// Subscription ids dropped by `Disconnect` since the last call.
    pub fn take_disconnected(&self) -> Vec<u64> {
        std::mem::take(&mut self.inner.lock().unwrap().disconnected)
    }

    pub fn subscriber_count(&self) -> usize {
        self.inner.lock().unwrap().subscribers.len()
    }

    /// Whether the next `send()` would have to block: true if any
    /// `Block`/`Disconnect`-policy subscriber's channel is currently full.
    /// `Lossy` subscribers never block, so they never count. A cheap,
    /// non-mutating peek — the cooperative scheduler uses this to skip a
    /// node whose output can't currently be sent without blocking, the same
    /// way it already skips a node whose input isn't ready, rather than
    /// calling `work()` and blocking the single cooperative thread inside a
    /// real send.
    pub fn would_block(&self) -> bool {
        let inner = self.inner.lock().unwrap();
        inner.subscribers.iter().any(|subscriber| {
            !matches!(subscriber.policy, OverflowPolicy::Lossy)
                && subscriber.tx.len() >= subscriber.tx.capacity().unwrap_or(usize::MAX)
        })
    }

    /// Broadcast one value. Non-blocking work happens under the lock;
    /// subscribers that need a *blocking* send are collected and served
    /// after it is released, so a stalled consumer never blocks a
    /// concurrent subscribe/unsubscribe.
    fn send(&self, value: &T) -> Result<(), ()> {
        self.send_message(ChannelMessage::Sample(value.clone()), Some(value))
    }

    fn send_batch(&self, values: &[T]) -> Result<(), ()> {
        if values.is_empty() {
            return Ok(());
        }
        self.send_message(ChannelMessage::Batch(values.to_vec()), values.last())
    }

    fn send_message(&self, message: ChannelMessage<T>, sticky_last: Option<&T>) -> Result<(), ()> {
        struct BlockedSend<T> {
            id: u64,
            tx: CrossbeamSender<ChannelMessage<T>>,
            deadline: Option<Duration>,
            message: ChannelMessage<T>,
        }

        let mut blocked: Vec<BlockedSend<T>> = Vec::new();
        {
            let mut inner = self.inner.lock().unwrap();
            if inner.sticky
                && let Some(last) = sticky_last
            {
                inner.last = Some(last.clone());
            }
            let mut dead: Vec<u64> = Vec::new();
            for subscriber in &mut inner.subscribers {
                // Lossy: retry the pending value first so values stay ordered.
                if let Some(pending) = subscriber.pending.take() {
                    match subscriber.tx.try_send(ChannelMessage::Sample(pending)) {
                        Ok(()) => {}
                        Err(TrySendError::Full(ChannelMessage::Sample(pending))) => {
                            // Still full: the new value supersedes it.
                            let _ = pending;
                        }
                        Err(TrySendError::Full(_)) => unreachable!("pending values are scalar"),
                        Err(TrySendError::Disconnected(_)) => {
                            dead.push(subscriber.id);
                            continue;
                        }
                    }
                }
                match subscriber.tx.try_send(message.clone()) {
                    Ok(()) => {}
                    Err(TrySendError::Full(message)) => match subscriber.policy {
                        OverflowPolicy::Block => blocked.push(BlockedSend {
                            id: subscriber.id,
                            tx: subscriber.tx.clone(),
                            deadline: None,
                            message,
                        }),
                        OverflowPolicy::Lossy => {
                            subscriber.pending = match message {
                                ChannelMessage::Sample(value) => Some(value),
                                ChannelMessage::Batch(values) => values.into_iter().last(),
                                ChannelMessage::EndOfStream => None,
                            };
                        }
                        OverflowPolicy::Disconnect(deadline) => blocked.push(BlockedSend {
                            id: subscriber.id,
                            tx: subscriber.tx.clone(),
                            deadline: Some(deadline),
                            message,
                        }),
                    },
                    Err(TrySendError::Disconnected(_)) => dead.push(subscriber.id),
                }
            }
            inner
                .subscribers
                .retain(|subscriber| !dead.contains(&subscriber.id));
        }

        for send in blocked {
            match send.deadline {
                None => {
                    if send.tx.send(send.message).is_err() {
                        let mut inner = self.inner.lock().unwrap();
                        inner.subscribers.retain(|s| s.id != send.id);
                    }
                }
                Some(deadline) => {
                    let result = send.tx.send_timeout(send.message, deadline);
                    if result.is_err() {
                        let mut inner = self.inner.lock().unwrap();
                        inner.subscribers.retain(|s| s.id != send.id);
                        inner.disconnected.push(send.id);
                    }
                }
            }
        }
        Ok(())
    }
}

/// Broadcast sender that sends to one or more consumers
///
/// Architecture: Direct broadcast from caller thread to all destinations.
/// For head-of-line blocking prevention, use `split_senders()` to get
/// individual senders and spawn one thread per destination in your node.
///
/// Includes watchdog monitoring to detect blocked sends.
pub struct Sender<T> {
    destinations: Vec<Destination<T>>,
    /// Live-path subscribers, read on every send (so subscriptions added
    /// mid-stream take effect immediately).
    shared: Option<SharedSenders<T>>,
    watchdog_handle: Option<WatchdogHandle>,
}

struct Destination<T> {
    tx: CrossbeamSender<ChannelMessage<T>>,
    label: Option<String>,
}

impl<T> Clone for Destination<T> {
    fn clone(&self) -> Self {
        Self {
            tx: self.tx.clone(),
            label: self.label.clone(),
        }
    }
}

impl<T: Clone + Send> Sender<T> {
    /// Create a new Sender from a vector of crossbeam senders
    pub fn new(destinations: Vec<CrossbeamSender<ChannelMessage<T>>>) -> Self {
        Self::new_labeled(destinations.into_iter().map(|tx| (tx, None)).collect())
    }

    pub(crate) fn new_labeled(
        destinations: Vec<(CrossbeamSender<ChannelMessage<T>>, Option<String>)>,
    ) -> Self {
        Self {
            destinations: destinations
                .into_iter()
                .map(|(tx, label)| Destination { tx, label })
                .collect(),
            shared: None,
            watchdog_handle: None,
        }
    }

    /// Create a Sender that broadcasts through a supervisor-owned
    /// subscriber list (live pipelines).
    pub fn from_shared(shared: SharedSenders<T>) -> Self {
        Self {
            destinations: Vec::new(),
            shared: Some(shared),
            watchdog_handle: None,
        }
    }

    /// Attach a watchdog handle to monitor send operations
    pub fn with_watchdog(&self, watchdog_handle: WatchdogHandle) -> Self {
        Self {
            destinations: self.destinations.clone(),
            shared: self.shared.clone(),
            watchdog_handle: Some(watchdog_handle),
        }
    }

    /// Split this broadcast sender into individual senders (one per destination)
    ///
    /// This allows nodes to spawn one thread per destination rather than
    /// broadcasting from a single thread. Each returned Sender sends to
    /// exactly one destination.
    ///
    /// For nodes that need per-destination parallelism (like DslFileSource),
    /// use this to get independent senders and spawn your own threads.
    ///
    /// # Example
    /// ```ignore
    /// // In a self-threading node's work() method:
    /// let senders = output_port.split_senders::<MyType>()?;
    /// for (idx, sender) in senders.into_iter().enumerate() {
    ///     thread::spawn(move || {
    ///         // Each thread sends to one destination independently
    ///         sender.send(data)?;
    ///     });
    /// }
    /// ```
    pub fn split_senders(&self) -> Vec<Sender<T>> {
        // A shared list is snapshotted at split time: self-threading sources
        // hand each destination to its own worker thread, so subscribers
        // added *after* the split are not seen by those threads. The live
        // compiler therefore treats new subscriptions directly on source
        // ports as a full-restart edit (deferred: per-block snapshots).
        let shared_snapshot = self.shared.iter().flat_map(|shared| {
            let inner = shared.inner.lock().unwrap();
            inner
                .subscribers
                .iter()
                .map(|subscriber| (subscriber.tx.clone(), subscriber.label.clone()))
                .collect::<Vec<_>>()
        });
        self.destinations
            .iter()
            .map(|destination| (destination.tx.clone(), destination.label.clone()))
            .chain(shared_snapshot)
            .map(|(tx, label)| Sender {
                destinations: vec![Destination { tx, label }],
                shared: None,
                watchdog_handle: self.watchdog_handle.clone(),
            })
            .collect()
    }

    pub fn destination_label(&self) -> Option<&str> {
        if self.destinations.len() == 1 && self.shared.is_none() {
            self.destinations[0].label.as_deref()
        } else {
            None
        }
    }

    /// Get the number of broadcast destinations
    pub fn num_destinations(&self) -> usize {
        self.destinations.len()
            + self
                .shared
                .as_ref()
                .map_or(0, SharedSenders::subscriber_count)
    }

    /// Send a value to all destinations
    ///
    /// Wraps the value in `ChannelMessage::Sample` and sends to all destinations
    /// sequentially with watchdog monitoring.
    /// If any destination blocks, the watchdog can detect it.
    ///
    /// For nodes that need non-blocking broadcast, use `split_senders()`
    /// to spawn one thread per destination.
    pub fn send(&self, value: T) -> Result<(), SendError<T>> {
        if self.destinations.is_empty() && self.shared.is_none() {
            return Ok(());
        }

        // Create watchdog guard if watchdog is attached
        let _guard = self.watchdog_handle.as_ref().map(OperationGuard::new);

        if let Some(shared) = &self.shared {
            let _ = shared.send(&value);
        }
        if self.destinations.is_empty() {
            return Ok(());
        }

        // Send to all destinations
        let mut any_success = false;
        let mut last_error = None;

        for dest in &self.destinations {
            match dest.tx.send(ChannelMessage::Sample(value.clone())) {
                Ok(()) => any_success = true,
                Err(SendError(msg)) => {
                    // Extract the inner value from the ChannelMessage for the error
                    if let ChannelMessage::Sample(v) = msg {
                        last_error = Some(SendError(v));
                    }
                }
            }
        }

        // Only fail if no destination succeeded
        if !any_success && let Some(e) = last_error {
            return Err(e);
        }

        Ok(())
    }

    /// Sends an ordered batch in one channel operation. `Receiver<T>`
    /// transparently flattens the envelope for scalar consumers.
    pub fn send_batch(&self, values: Vec<T>) -> Result<(), SendError<Vec<T>>> {
        if values.is_empty() || (self.destinations.is_empty() && self.shared.is_none()) {
            return Ok(());
        }

        let _guard = self.watchdog_handle.as_ref().map(OperationGuard::new);
        if let Some(shared) = &self.shared {
            let _ = shared.send_batch(&values);
        }
        if self.destinations.is_empty() {
            return Ok(());
        }

        let mut remaining = Some(values);
        let mut any_success = false;
        let mut last_error = None;
        let last_destination = self.destinations.len() - 1;
        for (index, destination) in self.destinations.iter().enumerate() {
            let batch = if index == last_destination {
                remaining
                    .take()
                    .expect("batch retained for last destination")
            } else {
                remaining.as_ref().expect("batch still available").clone()
            };
            match destination.tx.send(ChannelMessage::Batch(batch)) {
                Ok(()) => any_success = true,
                Err(SendError(ChannelMessage::Batch(batch))) => {
                    last_error = Some(SendError(batch));
                }
                Err(SendError(_)) => unreachable!("send_batch only sends Batch envelopes"),
            }
        }
        if !any_success && let Some(error) = last_error {
            return Err(error);
        }
        Ok(())
    }

    /// Signal end-of-stream to all destinations
    ///
    /// Sends `ChannelMessage::EndOfStream` to each destination, signaling that
    /// no more data will follow. Downstream `Receiver`s will return
    /// `WorkError::Shutdown` on subsequent `recv()`/`peek()` calls.
    ///
    /// Call this before dropping the sender when your node has finished
    /// producing data (especially for self-threading nodes using `split_senders()`).
    ///
    /// Note: a *shared* subscriber list is deliberately NOT closed here —
    /// its lifetime belongs to the supervisor (`SharedSenders::close`), so a
    /// node signaling "I'm done" doesn't tear down channels a restarted
    /// instance will reuse.
    pub fn close(&self) {
        let _guard = self.watchdog_handle.as_ref().map(OperationGuard::new);
        for dest in &self.destinations {
            let _ = dest.tx.send(ChannelMessage::EndOfStream);
        }
    }

    /// Try to send without blocking on the static destinations. Shared
    /// subscribers are served with their own per-subscription policies.
    pub fn try_send(&self, value: T) -> Result<(), crossbeam_channel::TrySendError<T>> {
        if let Some(shared) = &self.shared {
            let _ = shared.send(&value);
        }
        if self.destinations.is_empty() {
            return Ok(());
        }

        for dest in &self.destinations {
            dest.tx
                .try_send(ChannelMessage::Sample(value.clone()))
                .map_err(|e| match e {
                    crossbeam_channel::TrySendError::Full(msg) => {
                        if let ChannelMessage::Sample(v) = msg {
                            crossbeam_channel::TrySendError::Full(v)
                        } else {
                            unreachable!("we only send Sample here")
                        }
                    }
                    crossbeam_channel::TrySendError::Disconnected(msg) => {
                        if let ChannelMessage::Sample(v) = msg {
                            crossbeam_channel::TrySendError::Disconnected(v)
                        } else {
                            unreachable!("we only send Sample here")
                        }
                    }
                })?;
        }

        Ok(())
    }

    /// Check if this sender has any connected receivers. A shared list
    /// counts as connected even while momentarily empty — subscribers can
    /// join at any time.
    pub fn is_connected(&self) -> bool {
        !self.destinations.is_empty() || self.shared.is_some()
    }
}

impl<T: Clone> Clone for Sender<T> {
    fn clone(&self) -> Self {
        Self {
            destinations: self.destinations.clone(),
            shared: self.shared.clone(),
            watchdog_handle: self.watchdog_handle.clone(),
        }
    }
}

#[cfg(test)]
mod shared_tests {
    use super::*;
    use crate::sample::Sample;

    fn drain(rx: &crossbeam_channel::Receiver<ChannelMessage<Sample>>) -> Vec<Sample> {
        rx.try_iter()
            .filter_map(|message| match message {
                ChannelMessage::Sample(sample) => Some(sample),
                ChannelMessage::Batch(_) => None,
                ChannelMessage::EndOfStream => None,
            })
            .collect()
    }

    #[test]
    fn late_subscriber_is_primed_with_last_level() {
        let shared = SharedSenders::<Sample>::new(true);
        let sender = Sender::from_shared(shared.clone());
        let (_, early) = shared.subscribe(8, OverflowPolicy::Block);

        sender.send(Sample::new(true, 100)).unwrap();
        sender.send(Sample::new(false, 200)).unwrap();

        let (_, late) = shared.subscribe(8, OverflowPolicy::Block);
        sender.send(Sample::new(true, 300)).unwrap();

        assert_eq!(
            drain(&early),
            vec![
                Sample::new(true, 100),
                Sample::new(false, 200),
                Sample::new(true, 300)
            ]
        );
        // Primed with the level current at join time, then live traffic.
        assert_eq!(
            drain(&late),
            vec![Sample::new(false, 200), Sample::new(true, 300)]
        );
    }

    #[test]
    fn static_and_shared_senders_preserve_batch_envelopes() {
        let (tx, rx) = crossbeam_channel::bounded(2);
        Sender::new(vec![tx]).send_batch(vec![1, 2, 3]).unwrap();
        assert!(matches!(
            rx.recv(),
            Ok(ChannelMessage::Batch(values)) if values == vec![1, 2, 3]
        ));

        let shared = SharedSenders::<u32>::new(false);
        let sender = Sender::from_shared(shared.clone());
        let (_, rx) = shared.subscribe(2, OverflowPolicy::Block);
        sender.send_batch(vec![4, 5, 6]).unwrap();
        assert!(matches!(
            rx.recv(),
            Ok(ChannelMessage::Batch(values)) if values == vec![4, 5, 6]
        ));
    }

    #[test]
    fn lossy_batch_coalesces_to_its_latest_value() {
        let shared = SharedSenders::<u32>::new(false);
        let sender = Sender::from_shared(shared.clone());
        let (_, rx) = shared.subscribe(1, OverflowPolicy::Lossy);

        sender.send(1).unwrap();
        sender.send_batch(vec![2, 3, 4]).unwrap();
        assert!(matches!(rx.recv(), Ok(ChannelMessage::Sample(1))));
        sender.send(5).unwrap();
        assert!(matches!(rx.recv(), Ok(ChannelMessage::Sample(4))));
    }

    #[test]
    fn unsubscribe_disconnects_only_that_channel() {
        let shared = SharedSenders::<Sample>::new(false);
        let sender = Sender::from_shared(shared.clone());
        let (id_a, rx_a) = shared.subscribe(8, OverflowPolicy::Block);
        let (_, rx_b) = shared.subscribe(8, OverflowPolicy::Block);

        sender.send(Sample::new(true, 1)).unwrap();
        shared.unsubscribe(id_a);
        sender.send(Sample::new(false, 2)).unwrap();

        assert_eq!(drain(&rx_a), vec![Sample::new(true, 1)]);
        assert!(rx_a.recv().is_err(), "unsubscribed channel disconnects");
        assert_eq!(
            drain(&rx_b),
            vec![Sample::new(true, 1), Sample::new(false, 2)]
        );
    }

    #[test]
    fn close_sends_eos_and_rejects_late_joiners() {
        let shared = SharedSenders::<Sample>::new(false);
        let (_, rx) = shared.subscribe(8, OverflowPolicy::Block);
        shared.close();
        assert!(matches!(rx.recv(), Ok(ChannelMessage::EndOfStream)));

        let (_, late) = shared.subscribe(8, OverflowPolicy::Block);
        assert!(matches!(late.recv(), Ok(ChannelMessage::EndOfStream)));
    }

    #[test]
    fn lossy_subscriber_coalesces_to_latest() {
        let shared = SharedSenders::<Sample>::new(true);
        let sender = Sender::from_shared(shared.clone());
        let (_, rx) = shared.subscribe(1, OverflowPolicy::Lossy);

        sender.send(Sample::new(true, 1)).unwrap(); // fills the buffer
        sender.send(Sample::new(false, 2)).unwrap(); // pending
        sender.send(Sample::new(true, 3)).unwrap(); // supersedes pending

        assert_eq!(drain(&rx), vec![Sample::new(true, 1)]);
        // Consumer drained; the next send first delivers the pending latest.
        sender.send(Sample::new(false, 4)).unwrap();
        assert_eq!(
            drain(&rx),
            vec![Sample::new(true, 3)] // 2 was superseded; 4 pending now
        );
    }

    #[test]
    fn disconnect_policy_drops_the_laggard_and_reports_it() {
        let shared = SharedSenders::<Sample>::new(false);
        let sender = Sender::from_shared(shared.clone());
        let (id, rx) = shared.subscribe(1, OverflowPolicy::Disconnect(Duration::from_millis(20)));
        let (_, healthy) = shared.subscribe(8, OverflowPolicy::Block);

        sender.send(Sample::new(true, 1)).unwrap(); // fills laggard buffer
        sender.send(Sample::new(false, 2)).unwrap(); // times out → disconnect

        assert_eq!(shared.take_disconnected(), vec![id]);
        assert_eq!(shared.subscriber_count(), 1);
        // Laggard got the first value, then its channel disconnected.
        assert_eq!(drain(&rx), vec![Sample::new(true, 1)]);
        assert!(rx.recv().is_err());
        assert_eq!(
            drain(&healthy),
            vec![Sample::new(true, 1), Sample::new(false, 2)]
        );
    }

    #[test]
    fn would_block_reflects_subscriber_fullness() {
        let shared = SharedSenders::<Sample>::new(false);
        let sender = Sender::from_shared(shared.clone());
        let (_, rx) = shared.subscribe(2, OverflowPolicy::Block);

        assert!(!shared.would_block(), "empty channel never blocks");
        sender.send(Sample::new(true, 100)).unwrap();
        assert!(!shared.would_block(), "channel not yet full");
        sender.send(Sample::new(false, 200)).unwrap();
        assert!(shared.would_block(), "channel now full");

        // Draining frees room again.
        assert!(rx.try_recv().is_ok());
        assert!(!shared.would_block());
    }

    #[test]
    fn would_block_ignores_lossy_subscribers() {
        let shared = SharedSenders::<Sample>::new(false);
        let sender = Sender::from_shared(shared.clone());
        let (_, _rx) = shared.subscribe(1, OverflowPolicy::Lossy);

        sender.send(Sample::new(true, 100)).unwrap();
        sender.send(Sample::new(false, 200)).unwrap(); // would overflow a Block subscriber
        assert!(
            !shared.would_block(),
            "a Lossy subscriber never makes the sender block"
        );
    }
}
