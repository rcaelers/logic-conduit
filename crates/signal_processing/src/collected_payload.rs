//! Stable identities for payload types retained as derived data.
//!
//! The registry deliberately records identity only. Typed ingestion and query
//! factories are added once the collector moves beyond its built-in lane
//! representations; keeping that later contract separate prevents a plugin
//! from being advertised as collectable before it has storage semantics.

use std::any::TypeId;
use std::collections::HashMap;

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
}

/// Bidirectional identity registry for collected payload types.
///
/// `TypeId` selects a typed channel while the application runs. `stable_id`
/// is the durable identity for serialized graph and panel state. Registering
/// the same type and identifier is idempotent; every other collision fails.
#[derive(Clone, Debug, Default)]
pub struct CollectedPayloadRegistry {
    by_type: HashMap<TypeId, CollectedPayloadDescriptor>,
    by_stable_id: HashMap<String, TypeId>,
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
}

#[cfg(test)]
mod collected_payload_tests {
    use super::*;

    #[derive(Clone)]
    struct First;
    #[derive(Clone)]
    struct Second;

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
}
