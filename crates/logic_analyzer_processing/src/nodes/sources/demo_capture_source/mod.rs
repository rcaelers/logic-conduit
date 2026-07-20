//! Deterministic demo source node and its live-acquisition provider.

mod implementation;

#[cfg(not(target_arch = "wasm32"))]
mod live_acquisition;

pub use implementation::DemoCaptureSource;
#[cfg(not(target_arch = "wasm32"))]
pub use live_acquisition::{
    DeterministicFakeConfig, DeterministicFakeController, DeterministicFakeProvider,
    DeterministicTrigger, DeterministicTriggerCount, DeterministicTriggerCountMode,
    DeterministicTriggerLogic, DeterministicTriggerPredicate, DeterministicTriggerStage,
};
