//! Explicit, user-placed decoupling point (`docs/PIPELINE_DESIGN.md`, flow
//! control): a plain relay whose whole purpose is the *channel* feeding its
//! input, not anything this node itself does. Backpressure from a slow
//! consumer should genuinely propagate to its producer by default — but
//! when a branch must be deliberately decoupled from a slower sibling
//! sharing the same producer, a `Buffer` node makes that capacity choice
//! visible and user-configured instead of an invisible framework default.

use std::collections::VecDeque;

use crate::runtime::node::{InputPort, OutputPort, ProcessNode, WorkError, WorkResult};
use crate::runtime::ports::{PortDirection, PortSchema};

/// Relays every item it receives, unchanged, from `in` to `out`. Generic
/// over any payload type flowing through a compiled pipeline edge — the
/// bounded crossbeam channel feeding this node's input *is* the buffer;
/// this node has no queue of its own beyond what receiving needs.
pub struct BufferNode<T> {
    name: String,
    input_buffer: VecDeque<T>,
}

impl<T> BufferNode<T> {
    const BATCH_SIZE: usize = 65_536;

    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            input_buffer: VecDeque::new(),
        }
    }
}

impl<T: Send + Sync + Clone + 'static> ProcessNode for BufferNode<T> {
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
        vec![PortSchema::new::<T>("in", 0, PortDirection::Input)]
    }

    fn output_schema(&self) -> Vec<PortSchema> {
        vec![PortSchema::new::<T>("out", 0, PortDirection::Output)]
    }

    fn work(&mut self, inputs: &[InputPort], outputs: &[OutputPort]) -> WorkResult<usize> {
        let mut input = inputs
            .first()
            .and_then(|port| port.get::<T>(&mut self.input_buffer))
            .ok_or_else(|| WorkError::NodeError("Missing buffer input".to_string()))?;
        let output = outputs
            .first()
            .and_then(|port| port.get::<T>())
            .ok_or_else(|| WorkError::NodeError("Missing buffer output".to_string()))?;

        let first = input.recv()?;
        let mut batch = Vec::with_capacity(Self::BATCH_SIZE);
        batch.push(first);
        let _ = input.try_recv_many(&mut batch, Self::BATCH_SIZE - 1);
        let count = batch.len();
        output.send_batch(batch)?;
        Ok(count)
    }
}

#[cfg(test)]
mod tests {
    use crossbeam_channel::bounded;

    use super::*;
    use crate::runtime::sender::{ChannelMessage, Sender};
    use crate::runtime::watchdog::Watchdog;

    fn run_buffer(buffer: &mut BufferNode<u64>, items: &[u64]) -> Vec<u64> {
        let wd = Watchdog::new();
        let (tx, rx) = bounded::<ChannelMessage<u64>>(64);
        for &item in items {
            tx.send(ChannelMessage::Sample(item)).unwrap();
        }
        drop(tx);
        let inputs = [InputPort::new_with_watchdog(rx, &wd, "buffer", "in")];
        let (out_tx, out_rx) = bounded::<ChannelMessage<u64>>(64);
        let outputs = [OutputPort::new_with_watchdog(
            Sender::new(vec![out_tx]),
            &wd,
            "buffer",
            "out",
        )];

        loop {
            match buffer.work(&inputs, &outputs) {
                Ok(_) => {}
                Err(WorkError::Shutdown) => break,
                Err(e) => panic!("unexpected error: {e}"),
            }
        }
        out_rx
            .try_iter()
            .flat_map(|message| match message {
                ChannelMessage::Sample(value) => vec![value],
                ChannelMessage::Batch(values) => values,
                ChannelMessage::EndOfStream => Vec::new(),
            })
            .collect()
    }

    #[test]
    fn relays_every_item_unchanged_and_in_order() {
        let received = run_buffer(&mut BufferNode::new("buffer"), &[1, 2, 3, 4, 5]);
        assert_eq!(received, vec![1, 2, 3, 4, 5]);
    }

    #[test]
    fn empty_input_produces_no_output() {
        let received = run_buffer(&mut BufferNode::new("buffer"), &[]);
        assert_eq!(received, Vec::<u64>::new());
    }
}
