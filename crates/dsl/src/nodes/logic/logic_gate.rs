//! Configurable boolean gate over signal levels

use crate::runtime::node::{InputPort, OutputPort, ProcessNode, WorkError, WorkResult};
use crate::runtime::ports::{PortDirection, PortSchema};
use crate::runtime::sample::Sample;
use std::collections::VecDeque;
use tracing::{debug, warn};

/// Boolean operation of a [`LogicGate`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateOp {
    /// Single input, inverted.
    Not,
    And,
    Nand,
    Or,
    Nor,
    /// Parity over all inputs.
    Xor,
    Xnor,
}

impl GateOp {
    fn combine(&self, levels: &[bool]) -> bool {
        match self {
            GateOp::Not => !levels[0],
            GateOp::And => levels.iter().all(|&l| l),
            GateOp::Nand => !levels.iter().all(|&l| l),
            GateOp::Or => levels.iter().any(|&l| l),
            GateOp::Nor => !levels.iter().any(|&l| l),
            GateOp::Xor => levels.iter().filter(|&&l| l).count() % 2 == 1,
            GateOp::Xnor => levels.iter().filter(|&&l| l).count() % 2 == 0,
        }
    }
}

/// N-input boolean gate over `Sample` level streams.
///
/// Inputs: `in0..inN-1` — `Sample`
/// Output: `out` — `Sample`
///
/// Merges its inputs in **strict timestamp order**: it holds every input's
/// current level (initially false), keeps one pending edge per input, and
/// applies the globally earliest one, blocking on an input whose next edge
/// is unknown. Unlike trigger streams (SR latch), level streams make this
/// safe: an input either advances or closes, and its edges are totally
/// ordered — while a purely event-driven merge corrupts the output timeline
/// whenever input arrival skew is large (a raw source channel runs
/// megabytes ahead of a decode-derived control level, so its edges would be
/// consumed far past the other input's current position and late edges
/// clamped en masse). The cost is lag, not deadlock: the output advances at
/// the pace of the laggiest input, which is the accepted-lag model of the
/// level-stream contract (§3.1).
///
/// Emits only output changes; the initial output is emitted at t=0. An
/// input that shuts down keeps its last level for the remainder of the run.
pub struct LogicGate {
    name: String,
    op: GateOp,
    levels: Vec<bool>,
    started: bool,
    last_out: bool,
    last_emit_ts: u64,
    /// Pending (peeked) edge per input, not yet applied.
    heads: Vec<Option<Sample>>,
    /// Inputs whose channel has closed; their level persists.
    eos: Vec<bool>,
    buffers: Vec<VecDeque<Sample>>,
}

impl LogicGate {
    /// `num_inputs` must be 1 for [`GateOp::Not`], and ≥ 1 otherwise.
    pub fn new(op: GateOp, num_inputs: usize) -> Self {
        assert!(num_inputs >= 1, "LogicGate needs at least one input");
        assert!(
            op != GateOp::Not || num_inputs == 1,
            "NOT gate takes exactly one input"
        );
        Self {
            name: "logic_gate".to_string(),
            op,
            levels: vec![false; num_inputs],
            started: false,
            last_out: false,
            last_emit_ts: 0,
            heads: vec![None; num_inputs],
            eos: vec![false; num_inputs],
            buffers: (0..num_inputs).map(|_| VecDeque::new()).collect(),
        }
    }

    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }
}

impl ProcessNode for LogicGate {
    fn name(&self) -> &str {
        &self.name
    }

    fn num_inputs(&self) -> usize {
        self.levels.len()
    }

    fn num_outputs(&self) -> usize {
        1
    }

    fn input_schema(&self) -> Vec<PortSchema> {
        (0..self.levels.len())
            .map(|i| PortSchema::new::<Sample>(format!("in{i}"), i, PortDirection::Input))
            .collect()
    }

    fn output_schema(&self) -> Vec<PortSchema> {
        vec![PortSchema::new::<Sample>("out", 0, PortDirection::Output)]
    }

    fn work(&mut self, inputs: &[InputPort], outputs: &[OutputPort]) -> WorkResult<usize> {
        let output = outputs
            .first()
            .and_then(|port| port.get::<Sample>())
            .ok_or_else(|| WorkError::NodeError("Missing gate output".to_string()))?;

        if !self.started {
            self.started = true;
            self.last_out = self.op.combine(&self.levels);
            output.send(Sample::new(self.last_out, 0))?;
        }

        let mut receivers = Vec::with_capacity(self.buffers.len());
        for (index, buffer) in self.buffers.iter_mut().enumerate() {
            let receiver = inputs
                .get(index)
                .and_then(|port| port.get::<Sample>(buffer))
                .ok_or_else(|| WorkError::NodeError(format!("Missing gate input {index}")))?;
            receivers.push(receiver);
        }

        // Strict merge: every live input must have a pending edge before any
        // edge is applied — blocking on the lagging input is what keeps the
        // output timeline ordered under arbitrary arrival skew.
        for (index, receiver) in receivers.iter_mut().enumerate() {
            if self.heads[index].is_some() || self.eos[index] {
                continue;
            }
            match receiver.recv() {
                Ok(sample) => self.heads[index] = Some(sample),
                Err(WorkError::Shutdown) => self.eos[index] = true,
                Err(e) => return Err(e),
            }
        }

        // Apply the globally earliest pending edge (input order breaks ties).
        let next = self
            .heads
            .iter()
            .enumerate()
            .filter_map(|(index, head)| head.map(|sample| (index, sample)))
            .min_by_key(|(index, sample)| (sample.start_time, *index));
        let Some((index, sample)) = next else {
            return Err(WorkError::Shutdown); // all inputs closed and drained
        };
        self.heads[index] = None;

        self.levels[index] = sample.value;
        let out = self.op.combine(&self.levels);
        if out == self.last_out {
            return Ok(0);
        }
        let mut ts = sample.start_time;
        if ts < self.last_emit_ts {
            warn!(
                "[{}] out-of-order edge at {}ns clamped to {}ns",
                self.name, ts, self.last_emit_ts
            );
            ts = self.last_emit_ts;
        }
        self.last_out = out;
        self.last_emit_ts = ts;
        debug!("[{}] out={} at {}ns", self.name, out, ts);
        output.send(Sample::new(out, ts))?;
        Ok(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::sender::{ChannelMessage, Sender};
    use crate::runtime::watchdog::Watchdog;
    use crossbeam_channel::bounded;

    fn run_gate(
        gate: &mut LogicGate,
        input_edges: Vec<Vec<Sample>>,
    ) -> Vec<Sample> {
        let wd = Watchdog::new();
        let mut inputs = Vec::new();
        for (i, edges) in input_edges.iter().enumerate() {
            let (tx, rx) = bounded::<ChannelMessage<Sample>>(256);
            for edge in edges {
                tx.send(ChannelMessage::Sample(*edge)).unwrap();
            }
            drop(tx);
            inputs.push(InputPort::new_with_watchdog(
                rx,
                &wd,
                "gate",
                &format!("in{i}"),
            ));
        }
        let (out_tx, out_rx) = bounded::<ChannelMessage<Sample>>(256);
        let outputs = vec![OutputPort::new_with_watchdog(
            Sender::new(vec![out_tx]),
            &wd,
            "gate",
            "out",
        )];

        loop {
            match gate.work(&inputs, &outputs) {
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
    fn not_inverts_edges() {
        let edges = run_gate(
            &mut LogicGate::new(GateOp::Not, 1),
            vec![vec![Sample::new(true, 100), Sample::new(false, 200)]],
        );
        assert_eq!(
            edges,
            vec![
                Sample::new(true, 0), // NOT of initial false
                Sample::new(false, 100),
                Sample::new(true, 200),
            ]
        );
    }

    #[test]
    fn and_emits_only_on_conjunction_changes() {
        let edges = run_gate(
            &mut LogicGate::new(GateOp::And, 2),
            vec![
                vec![Sample::new(true, 100), Sample::new(false, 400)],
                vec![Sample::new(true, 200), Sample::new(false, 300)],
            ],
        );
        assert_eq!(
            edges,
            vec![
                Sample::new(false, 0),
                Sample::new(true, 200),  // both high
                Sample::new(false, 300), // in1 drops
            ]
        );
    }

    #[test]
    fn or_and_xor_combiners() {
        let edges = run_gate(
            &mut LogicGate::new(GateOp::Or, 2),
            vec![
                vec![Sample::new(true, 100)],
                vec![Sample::new(true, 200), Sample::new(false, 300)],
            ],
        );
        assert_eq!(edges, vec![Sample::new(false, 0), Sample::new(true, 100)]);

        let edges = run_gate(
            &mut LogicGate::new(GateOp::Xor, 2),
            vec![
                vec![Sample::new(true, 100)],
                vec![Sample::new(true, 200), Sample::new(false, 300)],
            ],
        );
        assert_eq!(
            edges,
            vec![
                Sample::new(false, 0),
                Sample::new(true, 100),  // 1,0
                Sample::new(false, 200), // 1,1
                Sample::new(true, 300),  // 1,0
            ]
        );
    }

    #[test]
    fn nand_initial_state_is_true() {
        let edges = run_gate(&mut LogicGate::new(GateOp::Nand, 2), vec![vec![], vec![]]);
        assert_eq!(edges, vec![Sample::new(true, 0)]);
    }

    /// The failure mode that motivated the strict merge: one input's edges
    /// are all buffered immediately (a raw source channel), the other's
    /// arrive late in wall-clock time but early in stream time (a
    /// decode-derived level). The gate must still produce the
    /// timestamp-ordered conjunction.
    #[test]
    fn strict_merge_survives_arrival_skew() {
        use std::time::Duration;

        let wd = Watchdog::new();
        // Fast input: fully available up front.
        let (fast_tx, fast_rx) = bounded::<ChannelMessage<Sample>>(256);
        for edge in [
            Sample::new(true, 100),
            Sample::new(false, 200),
            Sample::new(true, 300),
            Sample::new(false, 400),
        ] {
            fast_tx.send(ChannelMessage::Sample(edge)).unwrap();
        }
        drop(fast_tx);
        // Slow input: edges arrive with wall-clock delay.
        let (slow_tx, slow_rx) = bounded::<ChannelMessage<Sample>>(256);
        let feeder = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            slow_tx
                .send(ChannelMessage::Sample(Sample::new(true, 150)))
                .unwrap();
            std::thread::sleep(Duration::from_millis(50));
            slow_tx
                .send(ChannelMessage::Sample(Sample::new(false, 350)))
                .unwrap();
        });

        let inputs = vec![
            InputPort::new_with_watchdog(fast_rx, &wd, "gate", "in0"),
            InputPort::new_with_watchdog(slow_rx, &wd, "gate", "in1"),
        ];
        let (out_tx, out_rx) = bounded::<ChannelMessage<Sample>>(256);
        let outputs = vec![OutputPort::new_with_watchdog(
            Sender::new(vec![out_tx]),
            &wd,
            "gate",
            "out",
        )];

        let mut gate = LogicGate::new(GateOp::And, 2);
        loop {
            match gate.work(&inputs, &outputs) {
                Ok(_) => {}
                Err(WorkError::Shutdown) => break,
                Err(e) => panic!("unexpected error: {e}"),
            }
        }
        feeder.join().unwrap();

        let edges: Vec<Sample> = out_rx
            .try_iter()
            .filter_map(|m| match m {
                ChannelMessage::Sample(s) => Some(s),
                _ => None,
            })
            .collect();
        assert_eq!(
            edges,
            vec![
                Sample::new(false, 0),
                Sample::new(true, 150),  // fast high @100, slow high @150
                Sample::new(false, 200), // fast drops
                Sample::new(true, 300),  // fast high again, slow still high
                Sample::new(false, 350), // slow drops
            ]
        );
    }

    #[test]
    fn closed_input_holds_its_level() {
        // in0 goes high then its channel closes; in1 toggles afterwards.
        let edges = run_gate(
            &mut LogicGate::new(GateOp::And, 2),
            vec![
                vec![Sample::new(true, 100)],
                vec![Sample::new(true, 500), Sample::new(false, 600)],
            ],
        );
        assert_eq!(
            edges,
            vec![
                Sample::new(false, 0),
                Sample::new(true, 500),
                Sample::new(false, 600),
            ]
        );
    }
}
