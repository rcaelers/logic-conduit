use crate::runtime::node::{InputPort, OutputPort, ProcessNode, WorkError, WorkResult};
use crate::runtime::ports::{PortDirection, PortSchema};
use crate::runtime::sample::Sample;

/// In-memory UART source for demos and tests.
///
/// Emits an idle-high 8N1 RX edge stream for a fixed byte string, then shuts
/// down. Protocol decoding still happens downstream through `UartDecoder`.
pub struct UartDemoSource {
    name: String,
    message: Vec<u8>,
    baud: u64,
    first_start_ns: u64,
    emitted: bool,
}

impl UartDemoSource {
    pub fn new(message: impl Into<Vec<u8>>, baud: u64) -> Self {
        Self {
            name: "uart_demo_source".to_string(),
            message: message.into(),
            baud: baud.max(1),
            first_start_ns: 60_000,
            emitted: false,
        }
    }

    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    fn bit_ns(&self) -> u64 {
        (1_000_000_000.0 / self.baud as f64).round() as u64
    }

    fn samples(&self) -> Vec<Sample> {
        let bit_ns = self.bit_ns();
        let mut samples = vec![Sample::new(true, 0)];
        let mut raw_level = true;
        let mut frame_start = self.first_start_ns;

        for &byte in &self.message {
            let mut bits = Vec::with_capacity(10);
            bits.push(false);
            for bit in 0..8 {
                bits.push(((byte >> bit) & 1) == 1);
            }
            bits.push(true);

            for (bit_index, bit_value) in bits.into_iter().enumerate() {
                let timestamp = frame_start + bit_index as u64 * bit_ns;
                if raw_level != bit_value {
                    raw_level = bit_value;
                    samples.push(Sample::new(raw_level, timestamp));
                }
            }
            frame_start += 10 * bit_ns;
        }

        samples
    }
}

impl ProcessNode for UartDemoSource {
    fn name(&self) -> &str {
        &self.name
    }

    fn should_stop(&self) -> bool {
        self.emitted
    }

    fn num_inputs(&self) -> usize {
        0
    }

    fn num_outputs(&self) -> usize {
        1
    }

    fn output_schema(&self) -> Vec<PortSchema> {
        vec![PortSchema::new::<Sample>("rx", 0, PortDirection::Output)]
    }

    fn work(&mut self, _inputs: &[InputPort], outputs: &[OutputPort]) -> WorkResult<usize> {
        if self.emitted {
            return Err(WorkError::Shutdown);
        }
        let output = outputs
            .first()
            .and_then(|port| port.get::<Sample>())
            .ok_or_else(|| WorkError::NodeError("Missing rx output".to_string()))?;
        let samples = self.samples();
        for sample in &samples {
            output.send(*sample)?;
        }
        self.emitted = true;
        Ok(samples.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hello_edges_are_idle_high_and_finite() {
        let source = UartDemoSource::new(b"HELLO\n".to_vec(), 115_200);
        let samples = source.samples();
        assert_eq!(samples.first(), Some(&Sample::new(true, 0)));
        assert!(samples.iter().any(|sample| !sample.value));
        assert!(
            samples
                .windows(2)
                .all(|pair| pair[0].start_time_ns < pair[1].start_time_ns)
        );
    }
}
