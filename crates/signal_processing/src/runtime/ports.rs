//! Port-based API for ergonomic node connections
//!
//! Provides a Pipeline builder that manages channels automatically,
//! plus InputPort and OutputPort type-erased wrappers for channel endpoints.

use std::any::TypeId;

use super::protocol::ProtocolKind;
use super::receiver::Receiver;
use super::sample_kind::SampleKind;
use super::sender::Sender;
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
    /// Protocols this port can speak, most preferred first. Default:
    /// `[Stream]`, the guaranteed fallback every port supports — override
    /// via [`Self::with_protocols`] for a port that can also answer
    /// [`super::edge_query::EdgeQuery`] (see
    /// [`super::node::ProcessNode::edge_query`]).
    pub protocols: Vec<ProtocolKind>,
    /// Payload kinds (`Sample` vs `SampleBlock`) this *output* port can
    /// produce, most preferred first — see [`super::sample_kind::negotiate`].
    /// Default empty, meaning "not polymorphic — `type_id` is the only
    /// option," true of every port except a handful of raw-channel sources
    /// (`DslFileSource`, `LogicAnalyzerSource`). Input ports leave this
    /// empty too: negotiation always resolves an input to its own declared
    /// `type_id`, so there's no separate "accepted kinds" concept to
    /// declare on the consuming side.
    pub sample_kinds: Vec<SampleKind>,
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
            protocols: vec![ProtocolKind::Stream],
            sample_kinds: Vec::new(),
        }
    }

    /// Declares which protocols this port can speak, most preferred first.
    pub fn with_protocols(mut self, protocols: Vec<ProtocolKind>) -> Self {
        self.protocols = protocols;
        self
    }

    /// Declares which payload kinds this output port can produce, most
    /// preferred first.
    pub fn with_sample_kinds(mut self, sample_kinds: Vec<SampleKind>) -> Self {
        self.sample_kinds = sample_kinds;
        self
    }
}

// ────────────────────────────────────────────────────────────────────────────
// Type-erased port wrappers
// ────────────────────────────────────────────────────────────────────────────

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use crossbeam_channel::Receiver as CrossbeamReceiver;

use super::edge_query::EdgeQuery;
use super::sender::ChannelMessage;

/// Type-erased input port wrapping a Receiver<T>
pub struct InputPort {
    channel: Box<dyn std::any::Any + Send>,
    watchdog_handle: Option<WatchdogHandle>,
    eos_received: AtomicBool,
    /// Set when this connection negotiated [`super::protocol::ProtocolKind::EdgeQuery`]
    /// — see [`Self::edge_query`].
    edge_query: Option<Arc<dyn EdgeQuery>>,
}

impl InputPort {
    /// Create from type-erased box (for internal use by Pipeline).
    /// Watchdog must be attached via with_watchdog() before use.
    pub(crate) fn from_type_erased(channel: Box<dyn std::any::Any + Send>) -> Self {
        Self {
            channel,
            watchdog_handle: None,
            eos_received: AtomicBool::new(false),
            edge_query: None,
        }
    }

    /// Create an intentionally disconnected input. Typed `get()` calls will
    /// return `None`, matching an optional/unconnected port.
    pub fn disconnected() -> Self {
        Self::from_type_erased(Box::new(()))
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
            edge_query: None,
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

    /// Attach the negotiated `EdgeQuery` handle for this connection
    /// (internal use by `Pipeline::build`).
    pub(crate) fn with_edge_query(mut self, edge_query: Option<Arc<dyn EdgeQuery>>) -> Self {
        self.edge_query = edge_query;
        self
    }

    /// The negotiated random-access query handle for this connection, if
    /// it settled on [`super::protocol::ProtocolKind::EdgeQuery`] rather
    /// than the streamed-channel protocol. Nodes that can use it should
    /// prefer it over `get()`; a `None` here means this connection (or an
    /// unconnected port) only supports streaming.
    pub fn edge_query(&self) -> Option<Arc<dyn EdgeQuery>> {
        self.edge_query.clone()
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

/// Type-erased output port wrapping one or more `Sender<T>`s.
///
/// Usually exactly one concrete `T` (as many ports as exist today). A port
/// backed by a node that negotiated [`super::sample_kind::SampleKind`]
/// with more than one destination can hold *both* a `Sender<Sample>` and a
/// `Sender<SampleBlock>` at once — `SampleKind` is a closed two-variant
/// enum, so a `Vec` scanned linearly is simpler and cheaper here than a
/// `HashMap` would be, and this is only ever looked up at node-startup
/// time, not per-sample.
pub struct OutputPort {
    channels: Vec<(TypeId, Box<dyn std::any::Any + Send>)>,
    watchdog_handle: Option<WatchdogHandle>,
}

impl OutputPort {
    /// Create from a type-erased box holding a `Sender<T>` (for internal
    /// use by Pipeline). `payload_type` must be `TypeId::of::<T>()` — the
    /// *payload* type `get::<T>()`/`split_senders::<T>()` will look up by,
    /// **not** `channel`'s own `Any::type_id()` (which would be
    /// `TypeId::of::<Sender<T>>()`, the wrapper, always different from
    /// `T`). Watchdog must be attached via with_watchdog() before use.
    pub(crate) fn from_type_erased(
        payload_type: TypeId,
        channel: Box<dyn std::any::Any + Send>,
    ) -> Self {
        Self {
            channels: vec![(payload_type, channel)],
            watchdog_handle: None,
        }
    }

    /// Adds a second concretely-typed sender to this port (internal use by
    /// `Pipeline::build`/`PipelineManager` when a producer negotiated more
    /// than one `SampleKind` for the same logical port). Same `payload_type`
    /// caveat as [`Self::from_type_erased`].
    pub(crate) fn extend_type_erased(
        mut self,
        payload_type: TypeId,
        channel: Box<dyn std::any::Any + Send>,
    ) -> Self {
        self.channels.push((payload_type, channel));
        self
    }

    /// Create a new OutputPort with a watchdog (for testing).
    pub fn new_with_watchdog<T: Send + Clone + 'static>(
        sender: Sender<T>,
        watchdog: &Watchdog,
        node_name: &str,
        port_name: &str,
    ) -> Self {
        Self {
            channels: vec![(TypeId::of::<T>(), Box::new(sender))],
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

    fn find<T: 'static>(&self) -> Option<&Sender<T>> {
        self.channels
            .iter()
            .find(|(type_id, _)| *type_id == TypeId::of::<T>())
            .and_then(|(_, boxed)| boxed.downcast_ref::<Sender<T>>())
    }

    /// Get a Sender with automatic watchdog monitoring.
    /// Returns an owned sender (cheaply cloned from internal storage).
    ///
    /// Returns None if the port doesn't contain a Sender<T>.
    ///
    /// # Panics
    /// Panics if watchdog has not been attached to this port.
    pub fn get<T: Send + Clone + 'static>(&self) -> Option<Sender<T>> {
        let sender = self.find::<T>()?;
        let watchdog = self.watchdog_handle.as_ref().expect(
            "OutputPort.get() called before watchdog attached - this is a bug in the pipeline",
        );
        Some(sender.with_watchdog(watchdog.clone()))
    }

    /// Clone the underlying Sender for this port.
    /// Used by nodes that spawn their own worker threads (e.g., DslFileSource).
    pub fn clone_sender<T: Send + Clone + 'static>(&self) -> Option<Sender<T>> {
        self.find::<T>().cloned()
    }

    /// Split the underlying broadcast Sender into individual senders (one per destination).
    ///
    /// For nodes that need per-destination parallelism (e.g., DslFileSource),
    /// this allows spawning one thread per destination. Each returned Sender
    /// sends to exactly one destination.
    ///
    /// Returns None if the port doesn't contain a Sender<T>, or if the sender
    /// has no destinations. A port carrying more than one `SampleKind`
    /// (see the struct doc) is queried independently per `T` — each call
    /// only sees the destinations that negotiated that particular type.
    pub fn split_senders<T: Send + Clone + 'static>(&self) -> Option<Vec<Sender<T>>> {
        let sender = self.find::<T>()?;
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
