//! Store-following source used by a fixed graph during live capture.

use std::collections::HashSet;
use std::time::Duration;

use signal_processing::errors::{WorkError, WorkResult};
use signal_processing::ports::{InputPort, OutputPort, PortDirection, PortSchema};
use signal_processing::{
    CaptureChannelId, CaptureCursorItem, CaptureStoreCursor, ProcessNode, Sample, SampleBlock,
    SampleKind,
};

const CURSOR_WAIT: Duration = Duration::from_millis(10);

/// Runtime port mapping for one physical capture channel.
///
/// A concrete graph feature supplies this metadata because only that feature
/// knows how its UI outputs lower to runtime ports. Equal edge and block names
/// describe one polymorphic port; different names describe two distinct ports.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CaptureAnalysisChannel {
    pub channel: CaptureChannelId,
    pub edge_port: String,
    pub block_port: String,
}

impl CaptureAnalysisChannel {
    pub fn polymorphic(channel: CaptureChannelId, port: impl Into<String>) -> Self {
        let port = port.into();
        Self {
            channel,
            edge_port: port.clone(),
            block_port: port,
        }
    }

    pub fn separate(
        channel: CaptureChannelId,
        edge_port: impl Into<String>,
        block_port: impl Into<String>,
    ) -> Self {
        Self {
            channel,
            edge_port: edge_port.into(),
            block_port: block_port.into(),
        }
    }
}

#[derive(Clone, Debug)]
struct ChannelPorts {
    channel: CaptureChannelId,
    edge_output: usize,
    block_output: usize,
}

/// A graph source that follows an authoritative capture-store cursor.
///
/// It owns no acquisition queue. If a decoder or sink is slow, this node's
/// cursor simply falls behind the committed prefix while acquisition and the
/// store writer continue independently.
pub struct CaptureAnalysisSource {
    name: String,
    cursor: Box<dyn CaptureStoreCursor>,
    channels: Vec<ChannelPorts>,
    output_schema: Vec<PortSchema>,
    timestamp_step_ns: u64,
    last_levels: Vec<Option<bool>>,
    next_sample: u64,
    finished: bool,
}

impl CaptureAnalysisSource {
    pub fn new(
        name: impl Into<String>,
        cursor: Box<dyn CaptureStoreCursor>,
        sample_rate_hz: f64,
        channels: Vec<CaptureAnalysisChannel>,
    ) -> Result<Self, String> {
        if channels.is_empty() {
            return Err("live analysis requires at least one channel".into());
        }
        if !sample_rate_hz.is_finite() || sample_rate_hz <= 0.0 {
            return Err("live analysis sample rate must be positive".into());
        }
        let timestamp_step = (1_000_000_000.0 / sample_rate_hz).round();
        if !(1.0..=u64::MAX as f64).contains(&timestamp_step) {
            return Err(format!(
                "live analysis sample rate {sample_rate_hz} Hz cannot be represented by SampleBlock"
            ));
        }

        let mut channel_ids = HashSet::new();
        let mut port_names = HashSet::new();
        let mut output_schema = Vec::new();
        let mut channel_ports = Vec::with_capacity(channels.len());
        for channel in channels {
            if !channel_ids.insert(channel.channel.clone()) {
                return Err(format!(
                    "live analysis channel '{}' is configured more than once",
                    channel.channel
                ));
            }
            if channel.edge_port.is_empty() || channel.block_port.is_empty() {
                return Err("live analysis port names cannot be empty".into());
            }

            let edge_output = output_schema.len();
            let block_output;
            if channel.edge_port == channel.block_port {
                if !port_names.insert(channel.edge_port.clone()) {
                    return Err(format!(
                        "live analysis output port '{}' is configured more than once",
                        channel.edge_port
                    ));
                }
                output_schema.push(
                    PortSchema::new::<Sample>(
                        channel.edge_port.clone(),
                        edge_output,
                        PortDirection::Output,
                    )
                    .with_sample_kinds(vec![SampleKind::Block, SampleKind::Edge]),
                );
                block_output = edge_output;
            } else {
                if !port_names.insert(channel.edge_port.clone()) {
                    return Err(format!(
                        "live analysis output port '{}' is configured more than once",
                        channel.edge_port
                    ));
                }
                output_schema.push(PortSchema::new::<Sample>(
                    channel.edge_port,
                    edge_output,
                    PortDirection::Output,
                ));
                block_output = output_schema.len();
                if !port_names.insert(channel.block_port.clone()) {
                    return Err(format!(
                        "live analysis output port '{}' is configured more than once",
                        channel.block_port
                    ));
                }
                output_schema.push(PortSchema::new::<SampleBlock>(
                    channel.block_port,
                    block_output,
                    PortDirection::Output,
                ));
            }
            channel_ports.push(ChannelPorts {
                channel: channel.channel,
                edge_output,
                block_output,
            });
        }

        let channel_count = channel_ports.len();
        Ok(Self {
            name: name.into(),
            cursor,
            channels: channel_ports,
            output_schema,
            timestamp_step_ns: timestamp_step as u64,
            last_levels: vec![None; channel_count],
            next_sample: 0,
            finished: false,
        })
    }

    fn emit_chunk(
        &mut self,
        chunk: &signal_processing::CaptureChunk,
        outputs: &[OutputPort],
    ) -> WorkResult<usize> {
        if chunk.start_sample() != self.next_sample {
            return Err(WorkError::NodeError(format!(
                "live analysis chunk {} starts at {}, expected {}",
                chunk.sequence(),
                chunk.start_sample(),
                self.next_sample
            )));
        }
        if chunk.channels().len() != self.channels.len()
            || self
                .channels
                .iter()
                .zip(chunk.channels())
                .any(|(expected, actual)| &expected.channel != actual)
        {
            return Err(WorkError::NodeError(
                "live analysis chunk channel table does not match the source contract".into(),
            ));
        }
        let sample_count = usize::try_from(chunk.sample_count()).map_err(|_| {
            WorkError::NodeError("live analysis chunk is too large for this platform".into())
        })?;

        for (channel_index, channel) in self.channels.iter().enumerate() {
            if let Some(sender) = outputs
                .get(channel.block_output)
                .and_then(|output| output.get::<SampleBlock>())
                .filter(|sender| sender.num_destinations() != 0)
            {
                let mut packed = vec![0_u8; sample_count.div_ceil(8)];
                for relative in 0..sample_count {
                    if chunk
                        .packed_level(relative as u64, channel_index)
                        .expect("validated chunk channel and sample bounds")
                    {
                        packed[relative / 8] |= 1 << (relative % 8);
                    }
                }
                sender
                    .send(SampleBlock::new(
                        packed,
                        chunk.start_sample(),
                        sample_count,
                        self.timestamp_step_ns,
                    ))
                    .map_err(|_| {
                        WorkError::NodeError("live analysis block output disconnected".into())
                    })?;
            }

            if let Some(sender) = outputs
                .get(channel.edge_output)
                .and_then(|output| output.get::<Sample>())
                .filter(|sender| sender.num_destinations() != 0)
            {
                let mut edges = Vec::new();
                let mut last = self.last_levels[channel_index];
                for relative in 0..sample_count {
                    let value = chunk
                        .packed_level(relative as u64, channel_index)
                        .expect("validated chunk channel and sample bounds");
                    if last != Some(value) {
                        edges.push(Sample::new(
                            value,
                            chunk
                                .start_sample()
                                .saturating_add(relative as u64)
                                .saturating_mul(self.timestamp_step_ns),
                        ));
                        last = Some(value);
                    }
                }
                self.last_levels[channel_index] = last;
                sender.send_batch(edges).map_err(|_| {
                    WorkError::NodeError("live analysis edge output disconnected".into())
                })?;
            } else if sample_count != 0 {
                self.last_levels[channel_index] = chunk
                    .packed_level(sample_count as u64 - 1, channel_index)
                    .or(self.last_levels[channel_index]);
            }
        }

        self.next_sample = chunk.end_sample();
        Ok(sample_count)
    }

    fn finish_edges(&mut self, outputs: &[OutputPort]) -> WorkResult<()> {
        let end_ns = self.next_sample.saturating_mul(self.timestamp_step_ns);
        for (channel_index, channel) in self.channels.iter().enumerate() {
            let Some(value) = self.last_levels[channel_index] else {
                continue;
            };
            if let Some(sender) = outputs
                .get(channel.edge_output)
                .and_then(|output| output.get::<Sample>())
                .filter(|sender| sender.num_destinations() != 0)
            {
                sender.send(Sample::new(value, end_ns)).map_err(|_| {
                    WorkError::NodeError("live analysis edge output disconnected".into())
                })?;
            }
        }
        Ok(())
    }
}

impl ProcessNode for CaptureAnalysisSource {
    fn name(&self) -> &str {
        &self.name
    }

    fn should_stop(&self) -> bool {
        self.finished
    }

    fn num_inputs(&self) -> usize {
        0
    }

    fn num_outputs(&self) -> usize {
        self.output_schema.len()
    }

    fn output_schema(&self) -> Vec<PortSchema> {
        self.output_schema.clone()
    }

    fn work(&mut self, _inputs: &[InputPort], outputs: &[OutputPort]) -> WorkResult<usize> {
        match self
            .cursor
            .wait_next(CURSOR_WAIT)
            .map_err(|error| WorkError::NodeError(error.to_string()))?
        {
            CaptureCursorItem::Chunk(chunk) => self.emit_chunk(&chunk, outputs),
            CaptureCursorItem::Pending => Ok(0),
            CaptureCursorItem::End => {
                self.finish_edges(outputs)?;
                self.finished = true;
                Err(WorkError::Shutdown)
            }
        }
    }
}
