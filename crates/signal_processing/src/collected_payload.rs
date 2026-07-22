//! Stable identities for payload types retained as derived data.
//!
//! The registry deliberately records identity only. Typed ingestion and query
//! factories are added once the collector moves beyond its built-in lane
//! representations; keeping that later contract separate prevents a plugin
//! from being advertised as collectable before it has storage semantics.

use std::any::TypeId;
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

/// Context supplied when a payload adapter creates one retained lane.
#[derive(Clone)]
pub struct CollectedLaneRequest {
    name: String,
    input_index: usize,
    lanes: DerivedLanes,
    payload: CollectedPayloadDescriptor,
}

impl CollectedLaneRequest {
    pub fn new(
        name: impl Into<String>,
        input_index: usize,
        lanes: DerivedLanes,
        payload: CollectedPayloadDescriptor,
    ) -> Self {
        Self {
            name: name.into(),
            input_index,
            lanes,
            payload,
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
}
