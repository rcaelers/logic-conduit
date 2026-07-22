//! Conventional pipeline source for the DSLogic U3Pro16.

use signal_processing::{InputPort, OutputPort, ProcessNode, WorkResult};

use super::implementation::DsLogicU3Pro16;
use crate::support::logic_analyzer::{
    LogicAnalyzerResult, LogicAnalyzerSource, LogicCaptureConfig,
};

/// A DSLogic U3Pro16 source node for a conventional processing pipeline.
pub struct DsLogicU3Pro16Source {
    inner: LogicAnalyzerSource<DsLogicU3Pro16>,
}

impl DsLogicU3Pro16Source {
    /// Opens the first U3Pro16 and configures its pipeline source settings.
    pub fn open_first(config: LogicCaptureConfig) -> LogicAnalyzerResult<Self> {
        DsLogicU3Pro16::open_first()?
            .into_source(config)
            .map(|inner| Self { inner })
    }

    /// Assigns the pipeline node name.
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.inner = self.inner.with_name(name);
        self
    }
}

impl ProcessNode for DsLogicU3Pro16Source {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn should_stop(&self) -> bool {
        self.inner.should_stop()
    }

    fn is_self_threading(&self) -> bool {
        self.inner.is_self_threading()
    }

    fn num_inputs(&self) -> usize {
        self.inner.num_inputs()
    }

    fn num_outputs(&self) -> usize {
        self.inner.num_outputs()
    }

    fn output_schema(&self) -> Vec<signal_processing::PortSchema> {
        self.inner.output_schema()
    }

    fn work(&mut self, inputs: &[InputPort], outputs: &[OutputPort]) -> WorkResult<usize> {
        self.inner.work(inputs, outputs)
    }
}
