//! Type registry for dynamic channel creation

use super::sender::{ChannelMessage, Sender};
use crossbeam_channel::{Sender as CrossbeamSender, bounded};
use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// Type registry for creating channels dynamically based on TypeId
type ChannelCreatorFn =
    Box<dyn Fn(usize) -> (Box<dyn Any + Send>, Box<dyn Any + Send>) + Send + Sync>;
type OutputWrapperFn =
    Box<dyn Fn(Vec<Box<dyn Any + Send>>) -> Result<Box<dyn Any + Send>, String> + Send + Sync>;

pub(crate) struct TypeRegistry {
    channel_creators: HashMap<TypeId, ChannelCreatorFn>,
    output_wrappers: HashMap<TypeId, OutputWrapperFn>,
}

impl TypeRegistry {
    fn new() -> Self {
        Self {
            channel_creators: HashMap::new(),
            output_wrappers: HashMap::new(),
        }
    }

    /// Register a type for use in channels
    fn register<T: 'static + Send + Clone>(&mut self) {
        let type_id = TypeId::of::<T>();

        // Register channel creator — channels carry ChannelMessage<T> internally
        self.channel_creators.insert(
            type_id,
            Box::new(|buffer_size: usize| {
                let (tx, rx) = bounded::<ChannelMessage<T>>(buffer_size);
                (
                    Box::new(tx) as Box<dyn Any + Send>,
                    Box::new(rx) as Box<dyn Any + Send>,
                )
            }),
        );

        // Register output wrapper
        self.output_wrappers.insert(
            type_id,
            Box::new(|senders: Vec<Box<dyn Any + Send>>| {
                if senders.is_empty() {
                    return Err("No senders to wrap".to_string());
                }

                let mut typed_senders = Vec::new();
                for sender in senders {
                    match sender.downcast::<CrossbeamSender<ChannelMessage<T>>>() {
                        Ok(tx) => typed_senders.push(*tx),
                        Err(_) => return Err("Type mismatch in sender".to_string()),
                    }
                }

                // Create Sender without watchdog (will be attached by OutputPort)
                let broadcast_sender = Sender::new(typed_senders);

                Ok(Box::new(broadcast_sender) as Box<dyn Any + Send>)
            }),
        );
    }

    pub(crate) fn create_channel(
        &self,
        type_id: TypeId,
        buffer_size: usize,
    ) -> Option<(Box<dyn Any + Send>, Box<dyn Any + Send>)> {
        self.channel_creators
            .get(&type_id)
            .map(|creator| creator(buffer_size))
    }

    pub(crate) fn wrap_output(
        &self,
        type_id: TypeId,
        senders: Vec<Box<dyn Any + Send>>,
    ) -> Result<Box<dyn Any + Send>, String> {
        self.output_wrappers
            .get(&type_id)
            .ok_or_else(|| format!("Type {:?} not registered", type_id))?(senders)
    }
}

// Global type registry
lazy_static::lazy_static! {
    pub(crate) static ref TYPE_REGISTRY: Arc<Mutex<TypeRegistry>> = {
        let mut registry = TypeRegistry::new();

        // Register common types
        use crate::Sample;
        use crate::nodes::LogicChunk;
        use crate::runtime::sample::SampleBlock;
        use crate::nodes::decoders::{SpiTransfer, ParallelWord};
        registry.register::<Sample>();
        registry.register::<SampleBlock>();
        registry.register::<LogicChunk>();
        registry.register::<SpiTransfer>();
        registry.register::<ParallelWord>();

        Arc::new(Mutex::new(registry))
    };
}

/// Register a custom type for use in pipelines
/// Call this before building pipelines that use custom types
pub fn register_type<T: 'static + Send + Clone>() {
    TYPE_REGISTRY.lock().unwrap().register::<T>();
}
