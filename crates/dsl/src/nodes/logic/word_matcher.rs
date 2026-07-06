//! Word matcher — emits a trigger whenever a decoded word matches a pattern

use crate::nodes::decoders::{ParallelWord, SpiTransfer};
use crate::runtime::events::Trigger;
use crate::runtime::node::{InputPort, OutputPort, ProcessNode, WorkError, WorkResult};
use crate::runtime::ports::{PortDirection, PortSchema};
use crate::runtime::sample::Sample;
use std::collections::VecDeque;
use tracing::debug;

/// Which field of a word item the matcher compares. Word types without
/// multiple fields (e.g. [`ParallelWord`]) ignore this.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WordField {
    #[default]
    Mosi,
    Miso,
}

/// A decoded word stream item the matcher can consume.
pub trait WordSource: Send + Clone + 'static {
    fn word(&self, field: WordField) -> u64;
    /// Timestamp in nanoseconds.
    fn timestamp_ns(&self) -> u64;
}

impl WordSource for SpiTransfer {
    fn word(&self, field: WordField) -> u64 {
        match field {
            WordField::Mosi => self.mosi as u64,
            WordField::Miso => self.miso as u64,
        }
    }
    fn timestamp_ns(&self) -> u64 {
        self.timing.position
    }
}

impl WordSource for ParallelWord {
    fn word(&self, _field: WordField) -> u64 {
        self.value
    }
    fn timestamp_ns(&self) -> u64 {
        self.timing.position
    }
}

/// Comparison applied between the masked word and the masked pattern.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MatchOp {
    #[default]
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

impl MatchOp {
    fn matches(&self, word: u64, pattern: u64) -> bool {
        match self {
            MatchOp::Eq => word == pattern,
            MatchOp::Ne => word != pattern,
            MatchOp::Lt => word < pattern,
            MatchOp::Le => word <= pattern,
            MatchOp::Gt => word > pattern,
            MatchOp::Ge => word >= pattern,
        }
    }

    /// Parse from the wire names used by node configs ("eq", "ne", …).
    pub fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "eq" => MatchOp::Eq,
            "ne" => MatchOp::Ne,
            "lt" => MatchOp::Lt,
            "le" => MatchOp::Le,
            "gt" => MatchOp::Gt,
            "ge" => MatchOp::Ge,
            _ => return None,
        })
    }
}

/// Emits a [`Trigger`] for every word where `(word & mask) OP (pattern &
/// mask)` holds (`OP` = [`MatchOp`], `==` by default).
///
/// Inputs: `words` — a [`WordSource`] stream (`SpiTransfer` or `ParallelWord`)
/// Outputs: `trigger` — `Trigger` per match;
///          `matched` — optional `Sample` pulse lane for visualization
pub struct WordMatcher<T: WordSource> {
    name: String,
    pattern: u64,
    mask: u64,
    op: MatchOp,
    field: WordField,
    /// Width of the visualization pulse on the `matched` output.
    pulse_ns: u64,
    input_buffer: VecDeque<T>,
    matches: u64,
    /// End of the previously emitted pulse (monotonicity guard).
    last_pulse_end: u64,
    started: bool,
}

impl<T: WordSource> WordMatcher<T> {
    pub fn new(pattern: u64, mask: u64) -> Self {
        Self {
            name: "word_matcher".to_string(),
            pattern,
            mask,
            op: MatchOp::default(),
            field: WordField::default(),
            pulse_ns: 1_000,
            input_buffer: VecDeque::new(),
            matches: 0,
            last_pulse_end: 0,
            started: false,
        }
    }

    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    pub fn with_field(mut self, field: WordField) -> Self {
        self.field = field;
        self
    }

    pub fn with_op(mut self, op: MatchOp) -> Self {
        self.op = op;
        self
    }

    pub fn with_pulse_ns(mut self, pulse_ns: u64) -> Self {
        self.pulse_ns = pulse_ns.max(1);
        self
    }
}

impl<T: WordSource> ProcessNode for WordMatcher<T> {
    fn name(&self) -> &str {
        &self.name
    }

    fn num_inputs(&self) -> usize {
        1
    }

    fn num_outputs(&self) -> usize {
        2
    }

    fn input_schema(&self) -> Vec<PortSchema> {
        vec![PortSchema::new::<T>("words", 0, PortDirection::Input)]
    }

    /// Hot-appliable: `pattern` / `mask` (U64) and `field` ("mosi"/"miso").
    /// Takes effect for the next word; in-flight words already consumed keep
    /// the old match result (accepted §6.2 semantics).
    fn apply_config(
        &mut self,
        config: &crate::runtime::node::NodeConfig,
    ) -> crate::runtime::node::ConfigOutcome {
        use crate::runtime::node::{ConfigOutcome, ConfigValue};
        for (key, value) in config {
            match (key.as_str(), value) {
                ("pattern", ConfigValue::U64(pattern)) => self.pattern = *pattern,
                ("mask", ConfigValue::U64(mask)) => self.mask = *mask,
                ("field", ConfigValue::Text(field)) => {
                    self.field = if field == "miso" {
                        WordField::Miso
                    } else {
                        WordField::Mosi
                    };
                }
                ("op", ConfigValue::Text(op)) => match MatchOp::parse(op) {
                    Some(op) => self.op = op,
                    None => return ConfigOutcome::NeedsRestart,
                },
                _ => return ConfigOutcome::NeedsRestart,
            }
        }
        ConfigOutcome::Applied
    }

    fn output_schema(&self) -> Vec<PortSchema> {
        vec![
            PortSchema::new::<Trigger>("trigger", 0, PortDirection::Output),
            PortSchema::new::<Sample>("matched", 1, PortDirection::Output),
        ]
    }

    fn work(&mut self, inputs: &[InputPort], outputs: &[OutputPort]) -> WorkResult<usize> {
        let mut input = inputs
            .first()
            .and_then(|port| port.get::<T>(&mut self.input_buffer))
            .ok_or_else(|| WorkError::NodeError("Missing words input".to_string()))?;

        let trigger_out = outputs
            .first()
            .and_then(|port| port.get::<Trigger>())
            .ok_or_else(|| WorkError::NodeError("Missing trigger output".to_string()))?;
        // Optional visualization lane; None when unconnected.
        let pulse_out = outputs.get(1).and_then(|port| port.get::<Sample>());

        // Level-stream contract: the pulse lane is a level, low at t=0.
        if !self.started {
            self.started = true;
            if let Some(pulse) = &pulse_out {
                pulse.send(Sample::new(false, 0))?;
            }
        }

        let word = input.recv()?;
        let value = word.word(self.field);
        if self.op.matches(value & self.mask, self.pattern & self.mask) {
            let ts = word.timestamp_ns();
            self.matches += 1;
            debug!(
                "[{}] match #{}: 0x{:06X} at {}ns",
                self.name, self.matches, value, ts
            );
            trigger_out.send(Trigger::new(ts))?;
            if let Some(pulse) = &pulse_out {
                if ts >= self.last_pulse_end {
                    pulse.send(Sample::new(true, ts))?;
                    pulse.send(Sample::new(false, ts + self.pulse_ns))?;
                    self.last_pulse_end = ts + self.pulse_ns;
                } else {
                    debug!(
                        "[{}] pulse at {}ns overlaps previous (ends {}ns), skipped",
                        self.name, ts, self.last_pulse_end
                    );
                }
            }
            return Ok(1);
        }
        Ok(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nodes::decoders::TimingInfo;
    use crate::runtime::sender::{ChannelMessage, Sender};
    use crate::runtime::watchdog::Watchdog;
    use crossbeam_channel::bounded;

    fn transfer(mosi: u32, ts: u64) -> SpiTransfer {
        SpiTransfer {
            mosi,
            miso: 0,
            timing: TimingInfo::new(ts as f64 / 1_000.0, ts),
        }
    }

    fn run_to_shutdown(node: &mut dyn ProcessNode, inputs: &[InputPort], outputs: &[OutputPort]) {
        loop {
            match node.work(inputs, outputs) {
                Ok(_) => {}
                Err(WorkError::Shutdown) => break,
                Err(e) => panic!("unexpected error: {e}"),
            }
        }
    }

    #[test]
    fn matches_pattern_and_emits_triggers() {
        let wd = Watchdog::new();
        let (tx, rx) = bounded::<ChannelMessage<SpiTransfer>>(16);
        let input = InputPort::new_with_watchdog(rx, &wd, "m", "words");
        let (ttx, trx) = bounded::<ChannelMessage<Trigger>>(16);
        let trigger_out =
            OutputPort::new_with_watchdog(Sender::new(vec![ttx]), &wd, "m", "trigger");
        let (ptx, prx) = bounded::<ChannelMessage<Sample>>(16);
        let pulse_out = OutputPort::new_with_watchdog(Sender::new(vec![ptx]), &wd, "m", "matched");

        tx.send(ChannelMessage::Sample(transfer(0x600081, 100)))
            .unwrap();
        tx.send(ChannelMessage::Sample(transfer(0x600000, 200)))
            .unwrap();
        tx.send(ChannelMessage::Sample(transfer(0x600081, 300_000)))
            .unwrap();
        drop(tx);

        let mut m = WordMatcher::<SpiTransfer>::new(0x600081, 0xFFFFFF);
        run_to_shutdown(&mut m, &[input], &[trigger_out, pulse_out]);

        let triggers: Vec<Trigger> = trx
            .try_iter()
            .filter_map(|m| match m {
                ChannelMessage::Sample(t) => Some(t),
                _ => None,
            })
            .collect();
        assert_eq!(triggers, vec![Trigger::new(100), Trigger::new(300_000)]);

        let pulses: Vec<Sample> = prx
            .try_iter()
            .filter_map(|m| match m {
                ChannelMessage::Sample(s) => Some(s),
                _ => None,
            })
            .collect();
        // initial low + two true/false pulse pairs
        assert_eq!(pulses[0], Sample::new(false, 0));
        assert_eq!(pulses[1], Sample::new(true, 100));
        assert_eq!(pulses[2], Sample::new(false, 1_100));
        assert_eq!(pulses[3], Sample::new(true, 300_000));
    }

    #[test]
    fn mask_limits_comparison() {
        let wd = Watchdog::new();
        let (tx, rx) = bounded::<ChannelMessage<SpiTransfer>>(16);
        let input = InputPort::new_with_watchdog(rx, &wd, "m", "words");
        let (ttx, trx) = bounded::<ChannelMessage<Trigger>>(16);
        let trigger_out =
            OutputPort::new_with_watchdog(Sender::new(vec![ttx]), &wd, "m", "trigger");
        let (ptx, _prx) = bounded::<ChannelMessage<Sample>>(16);
        let pulse_out = OutputPort::new_with_watchdog(Sender::new(vec![ptx]), &wd, "m", "matched");

        // Match on register byte only (0x60xxxx)
        tx.send(ChannelMessage::Sample(transfer(0x600081, 1)))
            .unwrap();
        tx.send(ChannelMessage::Sample(transfer(0x600000, 2)))
            .unwrap();
        tx.send(ChannelMessage::Sample(transfer(0x6A0000, 3)))
            .unwrap();
        drop(tx);

        let mut m = WordMatcher::<SpiTransfer>::new(0x600000, 0xFF0000);
        run_to_shutdown(&mut m, &[input], &[trigger_out, pulse_out]);

        let triggers: Vec<u64> = trx
            .try_iter()
            .filter_map(|m| match m {
                ChannelMessage::Sample(t) => Some(t.timestamp_ns),
                _ => None,
            })
            .collect();
        assert_eq!(triggers, vec![1, 2]);
    }

    #[test]
    fn parallel_words_match_on_value() {
        let wd = Watchdog::new();
        let (tx, rx) = bounded::<ChannelMessage<ParallelWord>>(16);
        let input = InputPort::new_with_watchdog(rx, &wd, "m", "words");
        let (ttx, trx) = bounded::<ChannelMessage<Trigger>>(16);
        let trigger_out =
            OutputPort::new_with_watchdog(Sender::new(vec![ttx]), &wd, "m", "trigger");
        let (ptx, _prx) = bounded::<ChannelMessage<Sample>>(16);
        let pulse_out = OutputPort::new_with_watchdog(Sender::new(vec![ptx]), &wd, "m", "matched");

        for (v, ts) in [(0xAAu64, 10u64), (0x55, 20), (0xAA, 30)] {
            tx.send(ChannelMessage::Sample(ParallelWord {
                value: v,
                timing: TimingInfo::new(ts as f64 / 1_000.0, ts),
            }))
            .unwrap();
        }
        drop(tx);

        let mut m = WordMatcher::<ParallelWord>::new(0xAA, 0xFF);
        run_to_shutdown(&mut m, &[input], &[trigger_out, pulse_out]);

        let triggers: Vec<u64> = trx
            .try_iter()
            .filter_map(|m| match m {
                ChannelMessage::Sample(t) => Some(t.timestamp_ns),
                _ => None,
            })
            .collect();
        assert_eq!(triggers, vec![10, 30]);
    }

    #[test]
    fn inequality_ops_compare_masked_values() {
        let words: Vec<(u64, u64)> = vec![(0x10, 1), (0x20, 2), (0x30, 3)];
        let run_with_op = |op: MatchOp| -> Vec<u64> {
            let wd = Watchdog::new();
            let (tx, rx) = bounded::<ChannelMessage<ParallelWord>>(16);
            let input = InputPort::new_with_watchdog(rx, &wd, "m", "words");
            let (ttx, trx) = bounded::<ChannelMessage<Trigger>>(16);
            let trigger_out =
                OutputPort::new_with_watchdog(Sender::new(vec![ttx]), &wd, "m", "trigger");
            let (ptx, _prx) = bounded::<ChannelMessage<Sample>>(16);
            let pulse_out =
                OutputPort::new_with_watchdog(Sender::new(vec![ptx]), &wd, "m", "matched");
            for (v, ts) in &words {
                tx.send(ChannelMessage::Sample(ParallelWord {
                    value: *v,
                    timing: TimingInfo::new(*ts as f64 / 1_000.0, *ts),
                }))
                .unwrap();
            }
            drop(tx);
            let mut m = WordMatcher::<ParallelWord>::new(0x20, u64::MAX).with_op(op);
            run_to_shutdown(&mut m, &[input], &[trigger_out, pulse_out]);
            trx.try_iter()
                .filter_map(|m| match m {
                    ChannelMessage::Sample(t) => Some(t.timestamp_ns),
                    _ => None,
                })
                .collect()
        };

        assert_eq!(run_with_op(MatchOp::Eq), vec![2]);
        assert_eq!(run_with_op(MatchOp::Ne), vec![1, 3]);
        assert_eq!(run_with_op(MatchOp::Lt), vec![1]);
        assert_eq!(run_with_op(MatchOp::Le), vec![1, 2]);
        assert_eq!(run_with_op(MatchOp::Gt), vec![3]);
        assert_eq!(run_with_op(MatchOp::Ge), vec![2, 3]);
    }
}
