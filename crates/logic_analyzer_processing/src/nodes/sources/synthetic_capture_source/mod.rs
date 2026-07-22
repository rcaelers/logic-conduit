//! Deterministic synthetic source and its native acquisition provider.

mod implementation;

#[cfg(not(target_arch = "wasm32"))]
mod buffered_fake;
#[cfg(not(target_arch = "wasm32"))]
mod live_acquisition;

#[cfg(not(target_arch = "wasm32"))]
pub use buffered_fake::{BufferedFakeConfig, BufferedFakeController, BufferedFakeProvider};
pub use implementation::SyntheticCaptureSource;
#[cfg(not(target_arch = "wasm32"))]
pub use live_acquisition::{
    DeterministicFakeConfig, DeterministicFakeController, DeterministicFakeProvider,
    DeterministicTrigger, DeterministicTriggerCount, DeterministicTriggerCountMode,
    DeterministicTriggerLogic, DeterministicTriggerPredicate, DeterministicTriggerStage,
};
