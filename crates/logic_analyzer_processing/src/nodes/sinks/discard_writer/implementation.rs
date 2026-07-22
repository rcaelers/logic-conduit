//! Browser-safe writer stand-ins that consume streams without filesystem I/O.

use std::collections::VecDeque;

use signal_processing::{
    InputPort, OutputPort, PortDirection, PortSchema, ProcessNode, TextSample, Word, WorkError,
    WorkResult,
};

pub struct DiscardWordWriter {
    name: String,
    data: VecDeque<Word>,
    filenames: VecDeque<TextSample>,
}

impl DiscardWordWriter {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            data: VecDeque::new(),
            filenames: VecDeque::new(),
        }
    }
}

impl ProcessNode for DiscardWordWriter {
    fn name(&self) -> &str {
        &self.name
    }

    fn num_inputs(&self) -> usize {
        2
    }

    fn num_outputs(&self) -> usize {
        0
    }

    fn input_schema(&self) -> Vec<PortSchema> {
        vec![
            PortSchema::new::<Word>("data", 0, PortDirection::Input),
            PortSchema::new::<TextSample>("filename", 1, PortDirection::Input),
        ]
    }

    fn work(&mut self, inputs: &[InputPort], _outputs: &[OutputPort]) -> WorkResult<usize> {
        let mut data = inputs
            .first()
            .and_then(|port| port.get::<Word>(&mut self.data))
            .ok_or_else(|| WorkError::NodeError("Missing data input".to_owned()))?;
        data.recv()?;
        if let Some(mut filenames) = inputs
            .get(1)
            .and_then(|port| port.get::<TextSample>(&mut self.filenames))
        {
            while filenames.try_recv().is_ok() {}
        }
        Ok(1)
    }
}

pub struct DiscardTextWriter {
    name: String,
    lines: VecDeque<TextSample>,
    filenames: VecDeque<TextSample>,
}

impl DiscardTextWriter {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            lines: VecDeque::new(),
            filenames: VecDeque::new(),
        }
    }
}

impl ProcessNode for DiscardTextWriter {
    fn name(&self) -> &str {
        &self.name
    }

    fn num_inputs(&self) -> usize {
        2
    }

    fn num_outputs(&self) -> usize {
        0
    }

    fn input_schema(&self) -> Vec<PortSchema> {
        vec![
            PortSchema::new::<TextSample>("lines", 0, PortDirection::Input),
            PortSchema::new::<TextSample>("filename", 1, PortDirection::Input),
        ]
    }

    fn work(&mut self, inputs: &[InputPort], _outputs: &[OutputPort]) -> WorkResult<usize> {
        let mut lines = inputs
            .first()
            .and_then(|port| port.get::<TextSample>(&mut self.lines))
            .ok_or_else(|| WorkError::NodeError("Missing lines input".to_owned()))?;
        lines.recv()?;
        if let Some(mut filenames) = inputs
            .get(1)
            .and_then(|port| port.get::<TextSample>(&mut self.filenames))
        {
            while filenames.try_recv().is_ok() {}
        }
        Ok(1)
    }
}
