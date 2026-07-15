//! Connection protocol negotiation vocabulary.
//!
//! A connection between an output port and an input port can be carried by
//! more than one wire protocol. [`ProtocolKind`] names the protocols a port
//! can speak; [`super::pipeline::Pipeline::build`] negotiates the best
//! mutually-supported protocol per connection (producer preference order
//! wins ties) before allocating anything for it. Adding a new protocol is
//! adding a variant here plus producer/consumer support for it — the
//! negotiation logic itself never changes.

/// A wire protocol a port can speak on a connection, independent of the
/// Rust payload type it carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProtocolKind {
    /// Today's bounded crossbeam channel of `ChannelMessage<T>`. Every node
    /// supports this on every port — it is the guaranteed fallback a
    /// negotiation can never fail to find.
    Stream,
    /// Random-access `Arc<dyn EdgeQuery>` handle shared once at build time:
    /// no channel, no streaming, point/skip-ahead queries only.
    EdgeQuery,
}
