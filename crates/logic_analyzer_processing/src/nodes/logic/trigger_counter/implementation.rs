//! Trigger counter mapping trigger events into an integer level.

use std::collections::VecDeque;

use tracing::debug;

use signal_processing::{
    InputPort, NumberSample, OutputPort, PortDirection, PortSchema, ProcessNode, Trigger,
    WorkError, WorkResult,
};

/// Counts triggers into a [`NumberSample`] level: `start` at t=0, then
/// `start + n*step` after the n-th trigger.
///
/// Input: `trigger` — `Trigger`
/// Output: `count` — `NumberSample` level
pub struct TriggerCounter {
    name: String,
    start: i64,
    step: i64,
    count: i64,
    started: bool,
    input_buffer: VecDeque<Trigger>,
}

impl TriggerCounter {
    pub fn new(start: i64, step: i64) -> Self {
        Self {
            name: "trigger_counter".to_string(),
            start,
            step,
            count: 0,
            started: false,
            input_buffer: VecDeque::new(),
        }
    }

    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }
}

impl ProcessNode for TriggerCounter {
    fn name(&self) -> &str {
        &self.name
    }

    fn num_inputs(&self) -> usize {
        1
    }

    fn num_outputs(&self) -> usize {
        1
    }

    fn input_schema(&self) -> Vec<PortSchema> {
        vec![PortSchema::new::<Trigger>(
            "trigger",
            0,
            PortDirection::Input,
        )]
    }

    fn output_schema(&self) -> Vec<PortSchema> {
        vec![PortSchema::new::<NumberSample>(
            "count",
            0,
            PortDirection::Output,
        )]
    }

    fn work(&mut self, inputs: &[InputPort], outputs: &[OutputPort]) -> WorkResult<usize> {
        let mut input = inputs
            .first()
            .and_then(|port| port.get::<Trigger>(&mut self.input_buffer))
            .ok_or_else(|| WorkError::NodeError("Missing trigger input".to_string()))?;
        let output = outputs
            .first()
            .and_then(|port| port.get::<NumberSample>())
            .ok_or_else(|| WorkError::NodeError("Missing count output".to_string()))?;

        if !self.started {
            self.started = true;
            output.send(NumberSample::new(self.start, 0))?;
        }

        let trigger = input.recv()?;
        self.count += 1;
        let value = self.start + self.count * self.step;
        debug!(
            "[{}] count={} at {}ns",
            self.name, value, trigger.timestamp_ns
        );
        output.send(NumberSample::new(value, trigger.timestamp_ns))?;
        Ok(1)
    }
}

#[cfg(test)]
mod tests {
    use crossbeam_channel::bounded;
    use signal_processing::{ChannelMessage, Sender, Watchdog};

    use super::*;

    fn run_counter(counter: &mut TriggerCounter, triggers: &[u64]) -> Vec<NumberSample> {
        let wd = Watchdog::new();
        let (tx, rx) = bounded::<ChannelMessage<Trigger>>(64);
        for &ts in triggers {
            tx.send(ChannelMessage::Sample(Trigger::new(ts))).unwrap();
        }
        drop(tx);
        let inputs = [InputPort::new_with_watchdog(rx, &wd, "counter", "trigger")];
        let (out_tx, out_rx) = bounded::<ChannelMessage<NumberSample>>(64);
        let outputs = [OutputPort::new_with_watchdog(
            Sender::new(vec![out_tx]),
            &wd,
            "counter",
            "count",
        )];

        loop {
            match counter.work(&inputs, &outputs) {
                Ok(_) => {}
                Err(WorkError::Shutdown) => break,
                Err(e) => panic!("unexpected error: {e}"),
            }
        }
        out_rx
            .try_iter()
            .filter_map(|m| match m {
                ChannelMessage::Sample(s) => Some(s),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn counts_from_start_with_initial_level() {
        let levels = run_counter(&mut TriggerCounter::new(0, 1), &[100, 200, 300]);
        assert_eq!(
            levels,
            vec![
                NumberSample::new(0, 0),
                NumberSample::new(1, 100),
                NumberSample::new(2, 200),
                NumberSample::new(3, 300),
            ]
        );
    }

    #[test]
    fn custom_start_and_step() {
        let levels = run_counter(&mut TriggerCounter::new(10, 5), &[50]);
        assert_eq!(
            levels,
            vec![NumberSample::new(10, 0), NumberSample::new(15, 50)]
        );
    }

    #[test]
    fn no_triggers_emits_initial_only() {
        let levels = run_counter(&mut TriggerCounter::new(0, 1), &[]);
        assert_eq!(levels, vec![NumberSample::new(0, 0)]);
    }
}
