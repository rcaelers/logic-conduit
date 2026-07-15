//! SR flip-flop mapping set/reset triggers to a boolean level.

use std::collections::VecDeque;

use tracing::{debug, warn};

use signal_processing::errors::{WorkError, WorkResult};
use signal_processing::events::Trigger;
use signal_processing::node::ProcessNode;
use signal_processing::ports::{InputPort, OutputPort, PortDirection, PortSchema};
use signal_processing::sample::Sample;

/// Set/reset latch over [`Trigger`] streams.
///
/// Inputs: `set` (0), `reset` (1) — `Trigger`
/// Output: `q` — `Sample` level
///
/// The two ordered input streams are merged strictly by timestamp. Reset has
/// the higher input index, so a set and reset at the same instant net to
/// reset. The initial state is emitted at t=0.
pub struct SrLatch {
    name: String,
    initial: bool,
    state: bool,
    started: bool,
    last_emit_ts: u64,
    set_buffer: VecDeque<Trigger>,
    reset_buffer: VecDeque<Trigger>,
    heads: [Option<Trigger>; 2],
    eos: [bool; 2],
}

impl SrLatch {
    pub fn new(initial: bool) -> Self {
        Self {
            name: "sr_latch".to_string(),
            initial,
            state: initial,
            started: false,
            last_emit_ts: 0,
            set_buffer: VecDeque::new(),
            reset_buffer: VecDeque::new(),
            heads: [None, None],
            eos: [false, false],
        }
    }

    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }
}

impl ProcessNode for SrLatch {
    fn name(&self) -> &str {
        &self.name
    }

    fn num_inputs(&self) -> usize {
        2
    }

    fn num_outputs(&self) -> usize {
        1
    }

    fn input_schema(&self) -> Vec<PortSchema> {
        vec![
            PortSchema::new::<Trigger>("set", 0, PortDirection::Input),
            PortSchema::new::<Trigger>("reset", 1, PortDirection::Input),
        ]
    }

    fn output_schema(&self) -> Vec<PortSchema> {
        vec![PortSchema::new::<Sample>("q", 0, PortDirection::Output)]
    }

    fn work(&mut self, inputs: &[InputPort], outputs: &[OutputPort]) -> WorkResult<usize> {
        let output = outputs
            .first()
            .and_then(|port| port.get::<Sample>())
            .ok_or_else(|| WorkError::NodeError("Missing q output".to_string()))?;

        if !self.started {
            self.started = true;
            self.state = self.initial;
            output.send(Sample::new(self.state, 0))?;
        }

        let mut buffers = [&mut self.set_buffer, &mut self.reset_buffer];
        let mut receivers = Vec::with_capacity(2);
        for (index, buffer) in buffers.iter_mut().enumerate() {
            let receiver = inputs
                .get(index)
                .and_then(|port| port.get::<Trigger>(buffer))
                .ok_or_else(|| WorkError::NodeError("Missing set/reset input".to_string()))?;
            receivers.push(receiver);
        }

        // A head (or EOS) from every stream is required before choosing the
        // globally earliest event. Without this, scheduler/thread skew can
        // apply a later Set before an earlier Reset from the sibling branch.
        for (index, receiver) in receivers.iter_mut().enumerate() {
            if self.heads[index].is_some() || self.eos[index] {
                continue;
            }
            match receiver.recv() {
                Ok(trigger) => self.heads[index] = Some(trigger),
                Err(WorkError::Shutdown) => self.eos[index] = true,
                Err(error) => return Err(error),
            }
        }

        // (timestamp, input index): set (0) is applied before reset (1) at
        // ties, leaving reset as the net state.
        let next = self
            .heads
            .iter()
            .enumerate()
            .filter_map(|(index, trigger)| trigger.map(|trigger| (index, trigger)))
            .min_by_key(|(index, trigger)| (trigger.timestamp_ns, *index));
        let Some((index, trigger)) = next else {
            return Err(WorkError::Shutdown);
        };
        self.heads[index] = None;

        let new_state = index == 0;
        if new_state == self.state {
            return Ok(0);
        }
        let mut ts = trigger.timestamp_ns;
        if ts < self.last_emit_ts {
            warn!(
                "[{}] out-of-order {} at {}ns clamped to {}ns",
                self.name,
                if new_state { "set" } else { "reset" },
                ts,
                self.last_emit_ts
            );
            ts = self.last_emit_ts;
        }
        self.state = new_state;
        self.last_emit_ts = ts;
        debug!("[{}] q={} at {}ns", self.name, self.state, ts);
        output.send(Sample::new(self.state, ts))?;
        Ok(1)
    }
}

#[cfg(test)]
mod tests {
    use crossbeam_channel::bounded;
    use signal_processing::sender::{ChannelMessage, Sender};
    use signal_processing::watchdog::Watchdog;

    use super::*;

    struct Rig {
        set_tx: crossbeam_channel::Sender<ChannelMessage<Trigger>>,
        reset_tx: crossbeam_channel::Sender<ChannelMessage<Trigger>>,
        inputs: Vec<InputPort>,
        outputs: Vec<OutputPort>,
        q_rx: crossbeam_channel::Receiver<ChannelMessage<Sample>>,
    }

    fn rig() -> Rig {
        let wd = Watchdog::new();
        let (set_tx, set_rx) = bounded::<ChannelMessage<Trigger>>(64);
        let (reset_tx, reset_rx) = bounded::<ChannelMessage<Trigger>>(64);
        let (q_tx, q_rx) = bounded::<ChannelMessage<Sample>>(64);
        Rig {
            set_tx,
            reset_tx,
            inputs: vec![
                InputPort::new_with_watchdog(set_rx, &wd, "latch", "set"),
                InputPort::new_with_watchdog(reset_rx, &wd, "latch", "reset"),
            ],
            outputs: vec![OutputPort::new_with_watchdog(
                Sender::new(vec![q_tx]),
                &wd,
                "latch",
                "q",
            )],
            q_rx,
        }
    }

    /// Drops the senders (closing the channels), runs the latch until
    /// shutdown, and returns the emitted edges.
    fn run(rig: Rig, latch: &mut SrLatch) -> Vec<Sample> {
        let Rig {
            set_tx,
            reset_tx,
            inputs,
            outputs,
            q_rx,
        } = rig;
        drop(set_tx);
        drop(reset_tx);
        loop {
            match latch.work(&inputs, &outputs) {
                Ok(_) => {}
                Err(WorkError::Shutdown) => break,
                Err(e) => panic!("unexpected error: {e}"),
            }
        }
        q_rx.try_iter()
            .filter_map(|m| match m {
                ChannelMessage::Sample(s) => Some(s),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn set_reset_cycle() {
        let rig = rig();
        rig.set_tx
            .send(ChannelMessage::Sample(Trigger::new(100)))
            .unwrap();
        rig.reset_tx
            .send(ChannelMessage::Sample(Trigger::new(200)))
            .unwrap();
        rig.set_tx
            .send(ChannelMessage::Sample(Trigger::new(300)))
            .unwrap();

        let edges = run(rig, &mut SrLatch::new(false));
        assert_eq!(
            edges,
            vec![
                Sample::new(false, 0),
                Sample::new(true, 100),
                Sample::new(false, 200),
                Sample::new(true, 300),
            ]
        );
    }

    #[test]
    fn redundant_sets_do_not_emit() {
        let rig = rig();
        rig.set_tx
            .send(ChannelMessage::Sample(Trigger::new(100)))
            .unwrap();
        rig.set_tx
            .send(ChannelMessage::Sample(Trigger::new(150)))
            .unwrap();

        let edges = run(rig, &mut SrLatch::new(false));
        assert_eq!(edges, vec![Sample::new(false, 0), Sample::new(true, 100)]);
    }

    #[test]
    fn simultaneous_set_reset_nets_to_reset() {
        let rig = rig();
        rig.set_tx
            .send(ChannelMessage::Sample(Trigger::new(100)))
            .unwrap();
        rig.reset_tx
            .send(ChannelMessage::Sample(Trigger::new(100)))
            .unwrap();

        let edges = run(rig, &mut SrLatch::new(false));
        // Both edges are emitted at 100ns; the net state is false.
        assert_eq!(edges.first(), Some(&Sample::new(false, 0)));
        assert_eq!(edges.last(), Some(&Sample::new(false, 100)));
    }

    #[test]
    fn initial_state_true() {
        let edges = run(rig(), &mut SrLatch::new(true));
        assert_eq!(edges, vec![Sample::new(true, 0)]);
    }
}
