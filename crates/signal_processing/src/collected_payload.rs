//! Stable identities for payload types retained as derived data.
//!
//! The registry records durable payload identity and its typed ingestion
//! factory. Graph-level consumers decide separately whether a registered
//! payload is viewable, so collection and presentation remain independently
//! extensible.

use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::sync::Arc;

use crate::derived_data_collector::{DerivedDataRetention, DerivedLanes};
use crate::errors::WorkResult;
use crate::ports::{InputPort, PortSchema};

/// One type-erased collector input owned by a registered payload adapter.
///
/// Implementations downcast the input only to their registered payload type,
/// retain bounded data in their own storage, and publish an opaque query
/// handle through the [`CollectedLaneRequest`].
pub trait CollectedLaneIngestor: Send {
    fn input_schema(&self, index: usize) -> PortSchema;
    fn drain(&mut self, input: &InputPort, retention: DerivedDataRetention) -> WorkResult<usize>;
    fn is_finished(&self) -> bool;
}

/// Bounded visible-window request supplied to an adapter-owned retained query.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CollectedLaneSnapshotRequest {
    pub start_time_ns: u64,
    pub end_time_ns: u64,
    pub max_items: usize,
}

/// Revision and cardinality of one optional tabular lane projection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CollectedLaneTableMetadata {
    pub generation: u64,
    pub total_rows: u64,
}

/// One scalar record supplied by an optional tabular lane projection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CollectedLaneTableRow {
    pub start_time_ns: u64,
    pub end_time_ns: u64,
    pub value: u64,
}

/// Bounded rows for an optional scalar table projection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CollectedLaneTableSnapshot {
    pub rows: Vec<CollectedLaneTableRow>,
    pub complete: bool,
    pub format_hint: Option<String>,
}

/// Type-erased immutable result of a bounded retained-data query.
#[derive(Clone)]
pub struct OpaqueCollectedLaneSnapshot {
    value: Arc<dyn Any + Send + Sync>,
}

impl OpaqueCollectedLaneSnapshot {
    pub fn new<T: Send + Sync + 'static>(value: Arc<T>) -> Self {
        Self { value }
    }

    pub fn value<T: Send + Sync + 'static>(&self) -> Option<Arc<T>> {
        Arc::downcast::<T>(Arc::clone(&self.value)).ok()
    }
}

impl std::fmt::Debug for OpaqueCollectedLaneSnapshot {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OpaqueCollectedLaneSnapshot")
            .finish_non_exhaustive()
    }
}

/// Type-erased, adapter-owned retained-data query.
///
/// A collector publishes this after it has created the lane's storage. Data
/// subscribers may attach during or after a run and downcast it only to the
/// query type registered by that payload owner. The generic collector and
/// storage registry never inspect the concrete query value.
pub trait CollectedLaneQuery: Send + Sync {
    fn into_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync>;

    /// Produces an immutable, bounded snapshot for a visible window. The
    /// default declares that this query is panel-only and has no waveform
    /// representation.
    fn snapshot(
        &self,
        _request: CollectedLaneSnapshotRequest,
    ) -> Option<OpaqueCollectedLaneSnapshot> {
        None
    }

    /// Returns a nearby semantic time boundary for cursor snapping. The
    /// adapter defines which boundaries are meaningful for its payload; a
    /// query that has no snapping behavior returns `None`.
    fn nearest_time_boundary(&self, _timestamp_ns: u64, _max_distance_ns: u64) -> Option<u64> {
        None
    }

    /// Returns the greatest timeline timestamp retained by this lane. The
    /// adapter owns the exact span semantics for its payload; a query without
    /// timeline data returns `None`.
    fn timeline_extent_end_ns(&self) -> Option<u64> {
        None
    }

    /// Whether retained data can still change without replacing this query.
    fn is_live(&self) -> bool {
        false
    }

    /// Supplies revision metadata for a row-oriented scalar table. Queries
    /// without a table projection return `None`.
    fn table_metadata(&self) -> Option<CollectedLaneTableMetadata> {
        None
    }

    /// Supplies at most `max_rows` scalar table rows from the beginning of
    /// the retained sequence. `complete` reports whether more rows exist.
    fn table_snapshot(&self, _max_rows: usize) -> Option<CollectedLaneTableSnapshot> {
        None
    }
}

/// Context supplied when a payload adapter creates one retained lane.
#[derive(Clone)]
pub struct CollectedLaneRequest {
    name: String,
    input_index: usize,
    lanes: DerivedLanes,
    payload: CollectedPayloadDescriptor,
    retention: DerivedDataRetention,
    options: Arc<dyn Any + Send + Sync>,
}

impl CollectedLaneRequest {
    pub fn new(
        name: impl Into<String>,
        input_index: usize,
        lanes: DerivedLanes,
        payload: CollectedPayloadDescriptor,
        retention: DerivedDataRetention,
    ) -> Self {
        Self {
            name: name.into(),
            input_index,
            lanes,
            payload,
            retention,
            options: Arc::new(()),
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn input_index(&self) -> usize {
        self.input_index
    }

    pub fn lanes(&self) -> &DerivedLanes {
        &self.lanes
    }

    pub fn payload(&self) -> &CollectedPayloadDescriptor {
        &self.payload
    }

    pub fn retention(&self) -> DerivedDataRetention {
        self.retention
    }

    /// Attaches adapter-owned construction options without making the
    /// collector understand their concrete type.
    pub fn with_options<T: Send + Sync + 'static>(mut self, options: T) -> Self {
        self.options = Arc::new(options);
        self
    }

    pub fn options<T: Send + Sync + 'static>(&self) -> Option<&T> {
        self.options.downcast_ref::<T>()
    }

    /// Publishes an adapter-owned retained query under this lane's stable
    /// identity. Subscribers may resolve it immediately or after the
    /// producing run has finished.
    pub fn publish_query<T: CollectedLaneQuery + 'static>(&self, query: Arc<T>) {
        self.lanes
            .publish_opaque_lane(&self.name, self.payload.clone(), query);
    }
}

/// Factory for the typed ingestion and retained-query behavior of one payload.
pub trait CollectedPayloadAdapter: Send + Sync {
    fn create_ingestor(
        &self,
        request: CollectedLaneRequest,
    ) -> Result<Box<dyn CollectedLaneIngestor>, String>;
}

/// Persistable identity assigned by a payload owner.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CollectedPayloadDescriptor {
    stable_id: String,
}

impl CollectedPayloadDescriptor {
    /// Stable plugin-owned identity, suitable for saved state and diagnostics.
    pub fn stable_id(&self) -> &str {
        &self.stable_id
    }
}

/// Failure to add an ambiguous collected-payload identity.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum CollectedPayloadRegistrationError {
    #[error("collected payload identifiers must not be empty")]
    EmptyStableId,
    #[error(
        "payload type is already registered as '{existing_stable_id}', not '{requested_stable_id}'"
    )]
    TypeAlreadyRegistered {
        existing_stable_id: String,
        requested_stable_id: String,
    },
    #[error("collected payload identifier '{stable_id}' is already registered for another type")]
    StableIdAlreadyRegistered { stable_id: String },
    #[error("collected payload '{stable_id}' already has an ingestion adapter")]
    AdapterAlreadyRegistered { stable_id: String },
    #[error("payload type '{type_name}' has no collected-payload identity")]
    PayloadNotRegistered { type_name: String },
    #[error("collected payload '{stable_id}' has no ingestion adapter")]
    PayloadHasNoAdapter { stable_id: String },
}

/// Bidirectional identity registry for collected payload types.
///
/// `TypeId` selects a typed channel while the application runs. `stable_id`
/// is the durable identity for serialized graph and panel state. Registering
/// the same type and identifier is idempotent; every other collision fails.
#[derive(Clone, Default)]
pub struct CollectedPayloadRegistry {
    by_type: HashMap<TypeId, CollectedPayloadDescriptor>,
    by_stable_id: HashMap<String, TypeId>,
    adapters: HashMap<TypeId, Arc<dyn CollectedPayloadAdapter>>,
}

impl std::fmt::Debug for CollectedPayloadRegistry {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CollectedPayloadRegistry")
            .field("by_type", &self.by_type)
            .field("adapter_count", &self.adapters.len())
            .finish()
    }
}

impl CollectedPayloadRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register<T: Clone + Send + Sync + 'static>(
        &mut self,
        stable_id: impl Into<String>,
    ) -> Result<(), CollectedPayloadRegistrationError> {
        let stable_id = stable_id.into();
        if stable_id.trim().is_empty() {
            return Err(CollectedPayloadRegistrationError::EmptyStableId);
        }

        let type_id = TypeId::of::<T>();
        if let Some(existing) = self.by_type.get(&type_id) {
            return if existing.stable_id == stable_id {
                Ok(())
            } else {
                Err(CollectedPayloadRegistrationError::TypeAlreadyRegistered {
                    existing_stable_id: existing.stable_id.clone(),
                    requested_stable_id: stable_id,
                })
            };
        }
        if self.by_stable_id.contains_key(&stable_id) {
            return Err(CollectedPayloadRegistrationError::StableIdAlreadyRegistered { stable_id });
        }

        self.by_stable_id.insert(stable_id.clone(), type_id);
        self.by_type
            .insert(type_id, CollectedPayloadDescriptor { stable_id });
        Ok(())
    }

    pub fn descriptor<T: 'static>(&self) -> Option<&CollectedPayloadDescriptor> {
        self.descriptor_by_type_id(TypeId::of::<T>())
    }

    pub fn descriptor_by_type_id(&self, type_id: TypeId) -> Option<&CollectedPayloadDescriptor> {
        self.by_type.get(&type_id)
    }

    pub fn descriptor_by_stable_id(&self, stable_id: &str) -> Option<&CollectedPayloadDescriptor> {
        self.by_stable_id
            .get(stable_id)
            .and_then(|type_id| self.descriptor_by_type_id(*type_id))
    }

    /// Adds the typed ingestion factory for an already identified payload.
    pub fn register_adapter<T: Clone + Send + Sync + 'static>(
        &mut self,
        adapter: Arc<dyn CollectedPayloadAdapter>,
    ) -> Result<(), CollectedPayloadRegistrationError> {
        let type_id = TypeId::of::<T>();
        let Some(descriptor) = self.by_type.get(&type_id) else {
            return Err(CollectedPayloadRegistrationError::PayloadNotRegistered {
                type_name: std::any::type_name::<T>().to_owned(),
            });
        };
        if self.adapters.contains_key(&type_id) {
            return Err(
                CollectedPayloadRegistrationError::AdapterAlreadyRegistered {
                    stable_id: descriptor.stable_id.clone(),
                },
            );
        }
        self.adapters.insert(type_id, adapter);
        Ok(())
    }

    pub fn adapter_by_type_id(&self, type_id: TypeId) -> Option<&Arc<dyn CollectedPayloadAdapter>> {
        self.adapters.get(&type_id)
    }
}

#[cfg(test)]
mod collected_payload_tests {
    use super::*;

    #[derive(Clone)]
    struct First;
    #[derive(Clone)]
    struct Second;

    struct TestQuery(Vec<u64>);

    impl CollectedLaneQuery for TestQuery {
        fn into_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
            self
        }

        fn snapshot(
            &self,
            request: CollectedLaneSnapshotRequest,
        ) -> Option<OpaqueCollectedLaneSnapshot> {
            Some(OpaqueCollectedLaneSnapshot::new(Arc::new(
                self.0
                    .iter()
                    .copied()
                    .take(request.max_items)
                    .collect::<Vec<_>>(),
            )))
        }
    }

    struct FailingAdapter;

    impl CollectedPayloadAdapter for FailingAdapter {
        fn create_ingestor(
            &self,
            _request: CollectedLaneRequest,
        ) -> Result<Box<dyn CollectedLaneIngestor>, String> {
            Err("not used by registration test".to_owned())
        }
    }

    #[test]
    fn same_type_and_stable_id_is_idempotent() {
        let mut registry = CollectedPayloadRegistry::new();

        registry.register::<First>("org.example.first/v1").unwrap();
        registry.register::<First>("org.example.first/v1").unwrap();

        assert_eq!(
            registry.descriptor::<First>().unwrap().stable_id(),
            "org.example.first/v1"
        );
    }

    #[test]
    fn rejects_type_or_stable_id_collisions() {
        let mut registry = CollectedPayloadRegistry::new();
        registry.register::<First>("org.example.first/v1").unwrap();

        assert!(matches!(
            registry.register::<First>("org.example.renamed/v1"),
            Err(CollectedPayloadRegistrationError::TypeAlreadyRegistered { .. })
        ));
        assert!(matches!(
            registry.register::<Second>("org.example.first/v1"),
            Err(CollectedPayloadRegistrationError::StableIdAlreadyRegistered { .. })
        ));
    }

    #[test]
    fn registered_identity_accepts_one_typed_ingestion_adapter() {
        let mut registry = CollectedPayloadRegistry::new();
        registry.register::<First>("org.example.first/v1").unwrap();

        registry
            .register_adapter::<First>(Arc::new(FailingAdapter))
            .unwrap();

        assert!(registry.adapter_by_type_id(TypeId::of::<First>()).is_some());
        assert!(matches!(
            registry.register_adapter::<First>(Arc::new(FailingAdapter)),
            Err(CollectedPayloadRegistrationError::AdapterAlreadyRegistered { .. })
        ));
    }

    #[test]
    fn adapter_registration_requires_a_payload_identity() {
        let mut registry = CollectedPayloadRegistry::new();

        assert!(matches!(
            registry.register_adapter::<First>(Arc::new(FailingAdapter)),
            Err(CollectedPayloadRegistrationError::PayloadNotRegistered { .. })
        ));
    }

    #[test]
    fn request_publishes_an_adapter_owned_query() {
        let lanes = DerivedLanes::new();
        let mut registry = CollectedPayloadRegistry::new();
        registry.register::<First>("org.example.first/v1").unwrap();
        let request = CollectedLaneRequest::new(
            "first",
            0,
            lanes.clone(),
            registry.descriptor::<First>().unwrap().clone(),
            DerivedDataRetention::Unlimited,
        );

        request.publish_query(Arc::new(TestQuery(vec![1_u64, 2, 3])));

        let snapshot = lanes.opaque_lanes()[0]
            .snapshot(CollectedLaneSnapshotRequest {
                start_time_ns: 0,
                end_time_ns: 1,
                max_items: 2,
            })
            .unwrap();
        let values = snapshot.value::<Vec<u64>>().unwrap();
        assert_eq!(values.as_slice(), &[1, 2]);
    }
}
