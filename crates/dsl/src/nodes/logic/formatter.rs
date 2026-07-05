//! Text formatter — maps an integer level to a text level via a template

use crate::runtime::events::{NumberSample, TextSample};
use crate::runtime::node::{InputPort, OutputPort, ProcessNode, WorkError, WorkResult};
use crate::runtime::ports::{PortDirection, PortSchema};
use std::collections::VecDeque;

/// Substitutes `{n}` / `{n:0W}` (zero-padded to width W) in `template` with
/// `value`. Unrecognized braces are passed through verbatim.
fn format_template(template: &str, value: i64) -> String {
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
        if spec == "n" {
            result.push_str(&value.to_string());
        } else if let Some(width_spec) = spec.strip_prefix("n:0")
            && let Ok(width) = width_spec.parse::<usize>()
        {
            result.push_str(&format!("{value:0width$}"));
        } else {
            // Not ours — pass through including braces.
            result.push_str(&tail[..=close]);
        }
        rest = &tail[close + 1..];
    }
    result.push_str(rest);
    result
}

/// Stateless level map: every incoming [`NumberSample`] (including the t=0
/// initial value) becomes a [`TextSample`] with the template applied, at the
/// same timestamp — so the text level is defined from t=0 like its input.
///
/// Input: `value` — `NumberSample`
/// Output: `text` — `TextSample`
pub struct TextFormatter {
    name: String,
    template: String,
    input_buffer: VecDeque<NumberSample>,
}

impl TextFormatter {
    pub fn new(template: impl Into<String>) -> Self {
        Self {
            name: "text_formatter".to_string(),
            template: template.into(),
            input_buffer: VecDeque::new(),
        }
    }

    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }
}

impl ProcessNode for TextFormatter {
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
        vec![PortSchema::new::<NumberSample>(
            "value",
            0,
            PortDirection::Input,
        )]
    }

    fn output_schema(&self) -> Vec<PortSchema> {
        vec![PortSchema::new::<TextSample>(
            "text",
            0,
            PortDirection::Output,
        )]
    }

    fn work(&mut self, inputs: &[InputPort], outputs: &[OutputPort]) -> WorkResult<usize> {
        let mut input = inputs
            .first()
            .and_then(|port| port.get::<NumberSample>(&mut self.input_buffer))
            .ok_or_else(|| WorkError::NodeError("Missing value input".to_string()))?;
        let output = outputs
            .first()
            .and_then(|port| port.get::<TextSample>())
            .ok_or_else(|| WorkError::NodeError("Missing text output".to_string()))?;

        let number = input.recv()?;
        let text = format_template(&self.template, number.value);
        output.send(TextSample::new(text, number.start_time))?;
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
        assert_eq!(format_template("capture_{n}.bin", 7), "capture_7.bin");
        assert_eq!(
            format_template("out/capture_{n:04}.bin", 7),
            "out/capture_0007.bin"
        );
        assert_eq!(format_template("{n:04}_{n}", 42), "0042_42");
        assert_eq!(format_template("no placeholders", 1), "no placeholders");
        assert_eq!(format_template("odd {x} braces", 1), "odd {x} braces");
        assert_eq!(format_template("unclosed {n", 1), "unclosed {n");
        assert_eq!(format_template("{n:04}", -3), "-003");
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
        let texts: Vec<TextSample> = out_rx
            .try_iter()
            .filter_map(|m| match m {
                ChannelMessage::Sample(s) => Some(s),
                _ => None,
            })
            .collect();
        assert_eq!(
            texts,
            vec![
                TextSample::new("capture_0000.bin", 0),
                TextSample::new("capture_0001.bin", 500),
            ]
        );
    }
}
