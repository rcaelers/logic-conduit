//! Test-only adapters for driving graph contracts without exposing processing types.

use std::sync::Arc;
use std::time::Duration;

use signal_processing::{
    AcquisitionContext, AcquisitionResult, CaptureChannelId, CaptureProviderCapabilities,
    PreparedAcquisition, SimpleTriggerCondition,
};

#[derive(Clone, Debug)]
pub struct TestDeterministicFakeConfig(
    logic_analyzer_processing::test_support::DeterministicFakeConfig,
);

impl TestDeterministicFakeConfig {
    pub fn new(
        channels: impl Into<Arc<[CaptureChannelId]>>,
        chunk_sample_counts: impl Into<Arc<[u64]>>,
        seed: u64,
    ) -> AcquisitionResult<Self> {
        logic_analyzer_processing::test_support::DeterministicFakeConfig::new(
            channels,
            chunk_sample_counts,
            seed,
        )
        .map(Self)
    }

    pub fn with_simple_trigger(
        self,
        conditions: impl Into<Arc<[Option<SimpleTriggerCondition>]>>,
    ) -> AcquisitionResult<Self> {
        self.0.with_simple_trigger(conditions).map(Self)
    }

    pub fn total_samples(&self) -> u64 {
        self.0.total_samples()
    }

    pub fn first_trigger_sample(&self) -> Option<u64> {
        self.0.first_trigger_sample()
    }
}

#[derive(Clone, Debug)]
pub struct TestDeterministicFakeController(
    logic_analyzer_processing::test_support::DeterministicFakeController,
);

impl TestDeterministicFakeController {
    pub fn grant_chunks(&self, chunks: usize) {
        self.0.grant_chunks(chunks);
    }
}

pub struct TestDeterministicFakeProvider(
    logic_analyzer_processing::test_support::DeterministicFakeProvider,
);

impl TestDeterministicFakeProvider {
    pub fn manually_paced(
        config: TestDeterministicFakeConfig,
    ) -> (Self, TestDeterministicFakeController) {
        let (provider, controller) =
            logic_analyzer_processing::test_support::DeterministicFakeProvider::manually_paced(
                config.0,
            );
        (Self(provider), TestDeterministicFakeController(controller))
    }

    pub fn prepare(
        self,
        context: AcquisitionContext,
    ) -> AcquisitionResult<Box<dyn PreparedAcquisition>> {
        self.0.prepare(context)
    }
}

#[derive(Clone, Debug)]
pub struct TestBufferedFakeConfig(logic_analyzer_processing::test_support::BufferedFakeConfig);

impl TestBufferedFakeConfig {
    pub fn new(
        channels: impl Into<Arc<[CaptureChannelId]>>,
        sample_rate_hz: u64,
        total_samples: u64,
        upload_chunk_samples: u64,
        seed: u64,
    ) -> AcquisitionResult<Self> {
        logic_analyzer_processing::test_support::BufferedFakeConfig::new(
            channels,
            sample_rate_hz,
            total_samples,
            upload_chunk_samples,
            seed,
        )
        .map(Self)
    }

    pub fn with_simple_trigger(
        self,
        conditions: impl Into<Arc<[Option<SimpleTriggerCondition>]>>,
    ) -> AcquisitionResult<Self> {
        self.0.with_simple_trigger(conditions).map(Self)
    }

    pub fn capabilities(&self) -> &CaptureProviderCapabilities {
        self.0.capabilities()
    }

    pub fn first_trigger_sample(&self) -> Option<u64> {
        self.0.first_trigger_sample()
    }
}

#[derive(Clone, Debug)]
pub struct TestBufferedFakeController(
    logic_analyzer_processing::test_support::BufferedFakeController,
);

impl TestBufferedFakeController {
    pub fn wait_until_upload(&self, timeout: Duration) -> bool {
        self.0.wait_until_upload(timeout)
    }

    pub fn grant_upload_chunks(&self, chunks: usize) {
        self.0.grant_upload_chunks(chunks);
    }
}

pub struct TestBufferedFakeProvider(logic_analyzer_processing::test_support::BufferedFakeProvider);

impl TestBufferedFakeProvider {
    pub fn manually_uploaded(config: TestBufferedFakeConfig) -> (Self, TestBufferedFakeController) {
        let (provider, controller) =
            logic_analyzer_processing::test_support::BufferedFakeProvider::manually_uploaded(
                config.0,
            );
        (Self(provider), TestBufferedFakeController(controller))
    }

    pub fn prepare(
        self,
        context: AcquisitionContext,
    ) -> AcquisitionResult<Box<dyn PreparedAcquisition>> {
        self.0.prepare(context)
    }
}
