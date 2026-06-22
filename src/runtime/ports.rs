//! Port-based API for ergonomic node connections
//!
//! Provides a Pipeline builder that manages channels automatically,
//! plus InputPort and OutputPort type-erased wrappers for channel endpoints.

use std::any::TypeId;

// Re-export error types for backward compatibility
pub use super::errors::{ConnectionError, PortError};

// Re-export from submodules
pub use super::pipeline::Pipeline;
pub use super::receiver::Receiver;
pub use super::sender::Sender;
pub use super::type_registry::register_type;
pub use super::watchdog::{Watchdog, WatchdogHandle};

/// Direction of a port
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortDirection {
    Input,
    Output,
}

/// Schema describing a port's metadata
#[derive(Debug, Clone)]
pub struct PortSchema {
    pub name: String,
    pub type_id: TypeId,
    pub index: usize,
    pub direction: PortDirection,
}

impl PortSchema {
    /// Create a new port schema with type information
    pub fn new<T: 'static>(
        name: impl Into<String>,
        index: usize,
        direction: PortDirection,
    ) -> Self {
        Self {
            name: name.into(),
            type_id: TypeId::of::<T>(),
            index,
            direction,
        }
    }
}

// ────────────────────────────────────────────────────────────────────────────
// Type-erased port wrappers
// ────────────────────────────────────────────────────────────────────────────

use crossbeam_channel::Receiver as CrossbeamReceiver;
use std::fmt;
use std::sync::atomic::AtomicBool;

use super::sender::ChannelMessage;

/// Type-erased input port wrapping a Receiver<T>
pub struct InputPort {
    channel: Box<dyn std::any::Any + Send>,
    watchdog_handle: Option<WatchdogHandle>,
    eos_received: AtomicBool,
}

impl InputPort {
    /// Create from type-erased box (for internal use by Pipeline).
    /// Watchdog must be attached via with_watchdog() before use.
    pub(crate) fn from_type_erased(channel: Box<dyn std::any::Any + Send>) -> Self {
        Self {
            channel,
            watchdog_handle: None,
            eos_received: AtomicBool::new(false),
        }
    }

    /// Create a new InputPort with a watchdog (for testing).
    pub fn new_with_watchdog<T: Send + 'static>(
        receiver: CrossbeamReceiver<ChannelMessage<T>>,
        watchdog: &Watchdog,
        node_name: &str,
        port_name: &str,
    ) -> Self {
        Self {
            channel: Box::new(receiver),
            watchdog_handle: Some(watchdog.register_port(node_name, "recv", port_name)),
            eos_received: AtomicBool::new(false),
        }
    }

    /// Set watchdog context for this port
    pub(crate) fn with_watchdog(
        mut self,
        watchdog: Watchdog,
        node_name: String,
        port_name: String,
    ) -> Self {
        self.watchdog_handle = Some(watchdog.register_port(&node_name, "recv", &port_name));
        self
    }

    /// Get a Receiver with automatic watchdog monitoring.
    ///
    /// Returns None if the port doesn't contain a Receiver<T>.
    ///
    /// # Panics
    /// Panics if watchdog has not been attached to this port.
    pub fn get<'a, T: Send + 'static>(
        &'a self,
        buffer: &'a mut std::collections::VecDeque<T>,
    ) -> Option<Receiver<'a, T>> {
        let receiver = self
            .channel
            .downcast_ref::<CrossbeamReceiver<ChannelMessage<T>>>()?;
        let watchdog = self.watchdog_handle.as_ref().expect(
            "InputPort.get() called before watchdog attached - this is a bug in the pipeline",
        );
        Some(Receiver::with_watchdog(
            receiver,
            buffer,
            watchdog.clone(),
            &self.eos_received,
        ))
    }
}

/// Type-erased output port wrapping a Sender<T>
pub struct OutputPort {
    channel: Box<dyn std::any::Any + Send>,
    watchdog_handle: Option<WatchdogHandle>,
}

impl OutputPort {
    /// Create from type-erased box (for internal use by Pipeline).
    /// Watchdog must be attached via with_watchdog() before use.
    pub(crate) fn from_type_erased(channel: Box<dyn std::any::Any + Send>) -> Self {
        Self {
            channel,
            watchdog_handle: None,
        }
    }

    /// Create a new OutputPort with a watchdog (for testing).
    pub fn new_with_watchdog<T: Send + Clone + 'static>(
        sender: Sender<T>,
        watchdog: &Watchdog,
        node_name: &str,
        port_name: &str,
    ) -> Self {
        Self {
            channel: Box::new(sender),
            watchdog_handle: Some(watchdog.register_port(node_name, "send", port_name)),
        }
    }

    /// Set watchdog context for this port
    pub(crate) fn with_watchdog(
        mut self,
        watchdog: Watchdog,
        node_name: String,
        port_name: String,
    ) -> Self {
        self.watchdog_handle = Some(watchdog.register_port(&node_name, "send", &port_name));
        self
    }

    /// Get a Sender with automatic watchdog monitoring.
    /// Returns an owned sender (cheaply cloned from internal storage).
    ///
    /// Returns None if the port doesn't contain a Sender<T>.
    ///
    /// # Panics
    /// Panics if watchdog has not been attached to this port.
    pub fn get<T: Send + Clone + 'static>(&self) -> Option<Sender<T>> {
        let sender = self.channel.downcast_ref::<Sender<T>>()?;
        let watchdog = self.watchdog_handle.as_ref().expect(
            "OutputPort.get() called before watchdog attached - this is a bug in the pipeline",
        );
        Some(sender.with_watchdog(watchdog.clone()))
    }

    /// Clone the underlying Sender for this port.
    /// Used by nodes that spawn their own worker threads (e.g., DslFileSource).
    pub fn clone_sender<T: Send + Clone + 'static>(&self) -> Option<Sender<T>> {
        self.channel.downcast_ref::<Sender<T>>().cloned()
    }

    /// Split the underlying broadcast Sender into individual senders (one per destination).
    ///
    /// For nodes that need per-destination parallelism (e.g., DslFileSource),
    /// this allows spawning one thread per destination. Each returned Sender
    /// sends to exactly one destination.
    ///
    /// Returns None if the port doesn't contain a Sender<T>, or if the sender
    /// has no destinations.
    pub fn split_senders<T: Send + Clone + 'static>(&self) -> Option<Vec<Sender<T>>> {
        let sender = self.channel.downcast_ref::<Sender<T>>()?;
        let splits = sender.split_senders();
        if splits.is_empty() {
            None
        } else {
            Some(splits)
        }
    }
}

impl fmt::Debug for OutputPort {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "OutputPort")
    }
}
