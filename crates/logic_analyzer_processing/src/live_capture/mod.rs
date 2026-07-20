//! Portable live-acquisition lifecycle for concrete logic-analyzer providers.

mod analysis;
#[cfg(not(target_arch = "wasm32"))]
mod buffered_fake_native;
#[cfg(all(test, not(target_arch = "wasm32")))]
mod conformance_tests;
#[cfg(not(target_arch = "wasm32"))]
mod fake_native;
mod implementation;
#[cfg(not(target_arch = "wasm32"))]
mod u3pro16_buffered_native;
#[cfg(not(target_arch = "wasm32"))]
mod u3pro16_common_native;
#[cfg(not(target_arch = "wasm32"))]
mod u3pro16_streaming_native;

pub use analysis::{CaptureAnalysisChannel, CaptureAnalysisSource};
#[cfg(not(target_arch = "wasm32"))]
pub use buffered_fake_native::{BufferedFakeConfig, BufferedFakeController, BufferedFakeProvider};
#[cfg(not(target_arch = "wasm32"))]
pub use fake_native::{
    DeterministicFakeConfig, DeterministicFakeController, DeterministicFakeProvider,
    DeterministicTrigger, DeterministicTriggerCount, DeterministicTriggerCountMode,
    DeterministicTriggerLogic, DeterministicTriggerPredicate, DeterministicTriggerStage,
};
pub use implementation::{
    AcquisitionContext, AcquisitionError, AcquisitionOutcome, AcquisitionResult, LogicCaptureEvent,
    PreparedAcquisition,
};
#[cfg(not(target_arch = "wasm32"))]
pub use u3pro16_buffered_native::DsLogicU3Pro16BufferedProvider;
#[cfg(not(target_arch = "wasm32"))]
pub use u3pro16_streaming_native::DsLogicU3Pro16StreamingProvider;
