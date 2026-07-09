//! Text formatter — maps integer levels to a text level via a template

use crate::runtime::events::{NumberSample, TextSample};
use crate::runtime::node::{InputPort, OutputPort, ProcessNode, WorkError, WorkResult};
use crate::runtime::ports::{PortDirection, PortSchema};
use std::collections::VecDeque;

/// Substitutes value placeholders in `template`:
///
/// - `{0}`, `{1}`, … — input value by index
/// - `{0:04}` — zero-padded to the given width
/// - `{n}` / `{n:0W}` — legacy aliases for input 0
///
/// Unrecognized braces are passed through verbatim.
fn format_template(template: &str, values: &[i64]) -> String {
    fn parse_spec(spec: &str) -> Option<(usize, Option<usize>)> {
        let (index_part, width_part) = match spec.split_once(':') {
            None => (spec, None),
            Some((index, width)) => (index, Some(width)),
        };
        let index = if index_part == "n" {
            0
        } else {
            index_part.parse::<usize>().ok()?
        };
        let width = match width_part {
            None => None,
            // Widths use the zero-padded form ("04"); a bare ":" is not ours.
            Some(width) => Some(width.strip_prefix('0')?.parse::<usize>().ok()?),
        };
        Some((index, width))
    }

    let mut result = String::with_capacity(template.len() + 8);
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        result.push_str(&rest[..open]);
        let tail = &rest[open..];
        let Some(close) = tail.find('}') else {
            result.push_str(tail);
            return result;
        };
        let spec = &tail[1..close];
        match parse_spec(spec) {
            Some((index, width)) if index < values.len() => {
                let value = values[index];
                match width {
                    Some(width) => result.push_str(&format!("{value:0width$}")),
                    None => result.push_str(&value.to_string()),
                }
            }
            _ => {
                // Not ours (or out of range) — pass through including braces.
                result.push_str(&tail[..=close]);
            }
        }
        rest = &tail[close + 1..];
    }
    result.push_str(rest);
    result
}

/// Maps N integer levels to one text level.
///
/// Inputs: `value` (+ `value1`, `value2`, … when constructed with more) —
/// `NumberSample` levels
/// Output: `text` — `TextSample`
///
/// Single input keeps the original 1:1 mapping: every incoming sample
/// (including the t=0 initial) emits the formatted text at the same
/// timestamp. With several inputs the node merges them in strict timestamp
/// order (like [`LogicGate`](super::LogicGate)) — holding every input's
/// current value, initially 0 — and emits whenever the formatted text
/// changes.
pub struct TextFormatter {
    name: String,
    template: String,
    values: Vec<i64>,
    heads: Vec<Option<NumberSample>>,
    eos: Vec<bool>,
    last_text: Option<String>,
    buffers: Vec<VecDeque<NumberSample>>,
}

impl TextFormatter {
    pub fn new(template: impl Into<String>) -> Self {
        Self::with_num_values(template, 1)
    }

    /// A formatter over `num_values` input levels (`{0}`…`{num_values-1}`).
    pub fn with_num_values(template: impl Into<String>, num_values: usize) -> Self {
        let num_values = num_values.max(1);
        Self {
            name: "text_formatter".to_string(),
            template: template.into(),
            values: vec![0; num_values],
            heads: vec![None; num_values],
            eos: vec![false; num_values],
            last_text: None,
            buffers: (0..num_values).map(|_| VecDeque::new()).collect(),
        }
    }

    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    fn port_name(index: usize) -> String {
        if index == 0 {
            "value".to_string()
        } else {
            format!("value{index}")
        }
    }
}

impl ProcessNode for TextFormatter {
    fn name(&self) -> &str {
        &self.name
    }

    /// Hot-appliable: `template` (Text). Applies to the next value change;
    /// the current text level keeps the old formatting until then.
    fn apply_config(
        &mut self,
        config: &crate::runtime::node::NodeConfig,
    ) -> crate::runtime::node::ConfigOutcome {
        use crate::runtime::node::{ConfigOutcome, ConfigValue};
        for (key, value) in config {
            match (key.as_str(), value) {
                ("template", ConfigValue::Text(template)) => self.template = template.clone(),
                _ => return ConfigOutcome::NeedsRestart,
            }
        }
        ConfigOutcome::Applied
    }

    fn num_inputs(&self) -> usize {
        self.values.len()
    }

    fn num_outputs(&self) -> usize {
        1
    }

    fn input_schema(&self) -> Vec<PortSchema> {
        (0..self.values.len())
            .map(|index| {
                PortSchema::new::<NumberSample>(Self::port_name(index), index, PortDirection::Input)
            })
            .collect()
    }

    fn output_schema(&self) -> Vec<PortSchema> {
        vec![PortSchema::new::<TextSample>(
            "text",
            0,
            PortDirection::Output,
        )]
    }

    fn work(&mut self, inputs: &[InputPort], outputs: &[OutputPort]) -> WorkResult<usize> {
        let output = outputs
            .first()
            .and_then(|port| port.get::<TextSample>())
            .ok_or_else(|| WorkError::NodeError("Missing text output".to_string()))?;

        // Single input: pure 1:1 level map (the original behavior).
        if self.values.len() == 1 {
            let mut input = inputs
                .first()
                .and_then(|port| port.get::<NumberSample>(&mut self.buffers[0]))
                .ok_or_else(|| WorkError::NodeError("Missing value input".to_string()))?;
            let number = input.recv()?;
            let text = format_template(&self.template, &[number.value]);
            output.send(TextSample::new(text, number.start_time_ns))?;
            return Ok(1);
        }

        // Multiple inputs: strict timestamp merge over levels, like the
        // logic gate — block on the input whose next change is unknown.
        for index in 0..self.values.len() {
            if self.heads[index].is_some() || self.eos[index] {
                continue;
            }
            let mut input = inputs
                .get(index)
                .and_then(|port| port.get::<NumberSample>(&mut self.buffers[index]))
                .ok_or_else(|| WorkError::NodeError(format!("Missing value input {index}")))?;
            match input.recv() {
                Ok(sample) => self.heads[index] = Some(sample),
                Err(WorkError::Shutdown) => self.eos[index] = true,
                Err(e) => return Err(e),
            }
        }

        let next = self
            .heads
            .iter()
            .enumerate()
            .filter_map(|(index, head)| head.clone().map(|sample| (index, sample)))
            .min_by_key(|(index, sample)| (sample.start_time_ns, *index));
        let Some((index, sample)) = next else {
            return Err(WorkError::Shutdown);
        };
        self.heads[index] = None;
        self.values[index] = sample.value;

        let text = format_template(&self.template, &self.values);
        if self.last_text.as_deref() == Some(text.as_str()) {
            return Ok(0);
        }
        self.last_text = Some(text.clone());
        output.send(TextSample::new(text, sample.start_time_ns))?;
        Ok(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::sender::{ChannelMessage, Sender};
    use crate::runtime::watchdog::Watchdog;
    use crossbeam_channel::bounded;

    #[test]
    fn template_substitution() {
        assert_eq!(format_template("capture_{n}.bin", &[7]), "capture_7.bin");
        assert_eq!(
            format_template("out/capture_{n:04}.bin", &[7]),
            "out/capture_0007.bin"
        );
        assert_eq!(format_template("{n:04}_{n}", &[42]), "0042_42");
        assert_eq!(format_template("no placeholders", &[1]), "no placeholders");
        assert_eq!(format_template("odd {x} braces", &[1]), "odd {x} braces");
        assert_eq!(format_template("unclosed {n", &[1]), "unclosed {n");
        assert_eq!(format_template("{n:04}", &[-3]), "-003");
        // Indexed placeholders.
        assert_eq!(format_template("{0}-{1:03}", &[7, 9]), "7-009");
        assert_eq!(format_template("{2} missing", &[7, 9]), "{2} missing");
    }

    fn collect(
        out_rx: &crossbeam_channel::Receiver<ChannelMessage<TextSample>>,
    ) -> Vec<TextSample> {
        out_rx
            .try_iter()
            .filter_map(|m| match m {
                ChannelMessage::Sample(s) => Some(s),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn maps_levels_to_text() {
        let wd = Watchdog::new();
        let (tx, rx) = bounded::<ChannelMessage<NumberSample>>(16);
        tx.send(ChannelMessage::Sample(NumberSample::new(0, 0)))
            .unwrap();
        tx.send(ChannelMessage::Sample(NumberSample::new(1, 500)))
            .unwrap();
        drop(tx);
        let inputs = [InputPort::new_with_watchdog(rx, &wd, "fmt", "value")];
        let (out_tx, out_rx) = bounded::<ChannelMessage<TextSample>>(16);
        let outputs = [OutputPort::new_with_watchdog(
            Sender::new(vec![out_tx]),
            &wd,
            "fmt",
            "text",
        )];

        let mut formatter = TextFormatter::new("capture_{n:04}.bin");
        loop {
            match formatter.work(&inputs, &outputs) {
                Ok(_) => {}
                Err(WorkError::Shutdown) => break,
                Err(e) => panic!("unexpected error: {e}"),
            }
        }
        assert_eq!(
            collect(&out_rx),
            vec![
                TextSample::new("capture_0000.bin", 0),
                TextSample::new("capture_0001.bin", 500),
            ]
        );
    }

    #[test]
    fn merges_multiple_value_levels() {
        let wd = Watchdog::new();
        let (tx0, rx0) = bounded::<ChannelMessage<NumberSample>>(16);
        let (tx1, rx1) = bounded::<ChannelMessage<NumberSample>>(16);
        // Input 0: 0@0, 1@300. Input 1: 5@0, 6@200.
        for sample in [NumberSample::new(0, 0), NumberSample::new(1, 300)] {
            tx0.send(ChannelMessage::Sample(sample)).unwrap();
        }
        for sample in [NumberSample::new(5, 0), NumberSample::new(6, 200)] {
            tx1.send(ChannelMessage::Sample(sample)).unwrap();
        }
        drop(tx0);
        drop(tx1);
        let inputs = [
            InputPort::new_with_watchdog(rx0, &wd, "fmt", "value"),
            InputPort::new_with_watchdog(rx1, &wd, "fmt", "value1"),
        ];
        let (out_tx, out_rx) = bounded::<ChannelMessage<TextSample>>(16);
        let outputs = [OutputPort::new_with_watchdog(
            Sender::new(vec![out_tx]),
            &wd,
            "fmt",
            "text",
        )];

        let mut formatter = TextFormatter::with_num_values("{0}/{1}", 2);
        loop {
            match formatter.work(&inputs, &outputs) {
                Ok(_) => {}
                Err(WorkError::Shutdown) => break,
                Err(e) => panic!("unexpected error: {e}"),
            }
        }
        assert_eq!(
            collect(&out_rx),
            vec![
                TextSample::new("0/0", 0), // input 0's t=0 initial
                TextSample::new("0/5", 0), // input 1's t=0 initial
                TextSample::new("0/6", 200),
                TextSample::new("1/6", 300),
            ]
        );
    }
}
