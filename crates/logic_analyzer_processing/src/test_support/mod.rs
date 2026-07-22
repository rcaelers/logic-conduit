//! Test-only deterministic capture providers.

mod buffered_fake;
mod live_acquisition;

pub use buffered_fake::{BufferedFakeConfig, BufferedFakeController, BufferedFakeProvider};
pub use live_acquisition::{
    DeterministicFakeConfig, DeterministicFakeController, DeterministicFakeProvider,
    DeterministicTrigger, DeterministicTriggerCount, DeterministicTriggerCountMode,
    DeterministicTriggerLogic, DeterministicTriggerPredicate, DeterministicTriggerStage,
};
