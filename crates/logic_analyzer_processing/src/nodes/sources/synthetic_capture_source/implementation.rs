//! Small deterministic mixed-protocol capture used by platform stand-ins.

use signal_processing::{
    InputPort, OutputPort, PortDirection, PortSchema, ProcessNode, Sample, SampleBlock, WorkError,
    WorkResult,
};

const CHANNEL_COUNT: usize = 11;
const SAMPLE_COUNT: usize = 60_000;
const TIMESTAMP_STEP_NS: u64 = 1_000_000;
const CYCLE_SAMPLES: usize = 5_000;
const CYCLE_COUNT: usize = 12;

/// In-memory capture containing an eight-bit parallel bus and repeated SPI
/// transactions. Channels intentionally match the controlled-decoder
/// example: D0..D7 are Ch 0..7, SPI uses CS=8/CLK=7/MOSI=6/MISO=5, and the
/// parallel strobe is Ch 10. Twelve activity groups span a one-minute
/// timeline. SPI markers drive the graph's latch and parallel enable gate.
pub struct SyntheticCaptureSource {
    name: String,
    channel_count: usize,
    emitted: bool,
}

impl SyntheticCaptureSource {
    pub fn new() -> Self {
        Self {
            name: "synthetic_capture_source".to_owned(),
            channel_count: CHANNEL_COUNT,
            emitted: false,
        }
    }

    /// Uses the demo waveform for an arbitrary capture width. Channels past
    /// the authored demo lanes repeat deterministic waveforms, which keeps
    /// browser-only stand-ins useful for wide file and hardware sources.
    pub fn with_channel_count(mut self, channel_count: usize) -> Self {
        self.channel_count = channel_count.clamp(1, 32);
        self
    }

    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    /// Edge-form raw channels for displaying this generated capture before
    /// the processing graph starts.
    pub fn preview_channels() -> Vec<Vec<Sample>> {
        demo_channels()
            .iter()
            .map(|channel| edges(channel))
            .collect()
    }

    pub fn preview_channels_with_count(channel_count: usize) -> Vec<Vec<Sample>> {
        let channels = demo_channels();
        (0..channel_count.clamp(1, 32))
            .map(|channel| edges(&channels[channel % CHANNEL_COUNT]))
            .collect()
    }
}

impl Default for SyntheticCaptureSource {
    fn default() -> Self {
        Self::new()
    }
}

impl ProcessNode for SyntheticCaptureSource {
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
        self.channel_count * 2
    }

    fn output_schema(&self) -> Vec<PortSchema> {
        let mut schema = Vec::with_capacity(self.num_outputs());
        for channel in 0..self.channel_count {
            schema.push(PortSchema::new::<Sample>(
                format!("ch{channel}"),
                channel,
                PortDirection::Output,
            ));
        }
        for channel in 0..self.channel_count {
            schema.push(PortSchema::new::<SampleBlock>(
                format!("block{channel}"),
                self.channel_count + channel,
                PortDirection::Output,
            ));
        }
        schema
    }

    fn work(&mut self, _inputs: &[InputPort], outputs: &[OutputPort]) -> WorkResult<usize> {
        if self.emitted {
            return Err(WorkError::Shutdown);
        }

        let channels = demo_channels();
        for channel in 0..self.channel_count {
            let values = &channels[channel % CHANNEL_COUNT];
            if let Some(output) = outputs.get(channel).and_then(|port| port.get::<Sample>()) {
                output.send_batch(edges(values))?;
            }
            if let Some(output) = outputs
                .get(self.channel_count + channel)
                .and_then(|port| port.get::<SampleBlock>())
            {
                output.send(SampleBlock::new(
                    pack(values),
                    0,
                    SAMPLE_COUNT,
                    TIMESTAMP_STEP_NS,
                ))?;
            }
        }
        self.emitted = true;
        Ok(SAMPLE_COUNT)
    }
}

fn demo_channels() -> Vec<Vec<bool>> {
    let mut channels = vec![vec![false; SAMPLE_COUNT]; CHANNEL_COUNT];
    set_range(&mut channels[8], 0, SAMPLE_COUNT, true); // CS idle / bus enabled

    for cycle in 0..CYCLE_COUNT {
        let base = cycle * CYCLE_SAMPLES;
        add_parallel_burst(
            &mut channels,
            base + 80,
            &[0x10, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef],
        );
        add_spi_transaction(
            &mut channels,
            base + 440,
            &[0x9a, 0xbc, 0x42],
            &[0x11, 0x22, 0x33],
        );
        add_parallel_burst(
            &mut channels,
            base + 760,
            &[0x55, 0xaa, 0x0f, 0xf0, 0x3c, 0xc3, 0x5a, 0xa5],
        );
        add_spi_transaction(&mut channels, base + 1_160, &[0xde, 0xad], &[0xbe, 0xef]);
        add_parallel_burst(
            &mut channels,
            base + 1_440,
            &[0x01, 0x02, 0x04, 0x08, 0x10, 0x20, 0x40, 0x80],
        );
    }
    // Give raw viewers an explicit endpoint at almost exactly one minute.
    set_range(&mut channels[10], SAMPLE_COUNT - 2, SAMPLE_COUNT - 1, true);
    channels
}

fn add_parallel_burst(channels: &mut [Vec<bool>], start: usize, words: &[u8]) {
    const WORD_SAMPLES: usize = 24;
    for (index, word) in words.iter().copied().enumerate() {
        let word_start = start + index * WORD_SAMPLES;
        for (bit, channel) in channels.iter_mut().take(8).enumerate() {
            set_range(
                channel,
                word_start,
                word_start + WORD_SAMPLES,
                word & (1 << bit) != 0,
            );
        }
        set_range(&mut channels[10], word_start + 8, word_start + 14, true);
    }
}

fn add_spi_transaction(channels: &mut [Vec<bool>], start: usize, mosi: &[u8], miso: &[u8]) {
    const BIT_SAMPLES: usize = 10;
    assert_eq!(mosi.len(), miso.len());
    let end = start + mosi.len() * 8 * BIT_SAMPLES + 16;
    set_range(&mut channels[8], start, end, false);

    for (word_index, (&mosi_word, &miso_word)) in mosi.iter().zip(miso).enumerate() {
        for bit_index in 0..8 {
            let bit_start = start + 8 + (word_index * 8 + bit_index) * BIT_SAMPLES;
            let mask = 1 << (7 - bit_index);
            set_range(
                &mut channels[6],
                bit_start,
                bit_start + BIT_SAMPLES,
                mosi_word & mask != 0,
            );
            set_range(
                &mut channels[5],
                bit_start,
                bit_start + BIT_SAMPLES,
                miso_word & mask != 0,
            );
            set_range(&mut channels[7], bit_start + 3, bit_start + 7, true);
        }
    }
}

fn set_range(channel: &mut [bool], start: usize, end: usize, value: bool) {
    channel[start..end].fill(value);
}

fn edges(values: &[bool]) -> Vec<Sample> {
    let mut result = vec![Sample::new(values[0], 0)];
    for (position, pair) in values.windows(2).enumerate() {
        if pair[0] != pair[1] {
            result.push(Sample::new(
                pair[1],
                (position as u64 + 1) * TIMESTAMP_STEP_NS,
            ));
        }
    }
    result
}

fn pack(values: &[bool]) -> Vec<u8> {
    let mut packed = vec![0; values.len().div_ceil(8)];
    for (position, value) in values.iter().copied().enumerate() {
        if value {
            packed[position / 8] |= 1 << (position % 8);
        }
    }
    packed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_spans_one_minute_with_repeated_activity() {
        let channels = demo_channels();
        assert_eq!(channels.len(), CHANNEL_COUNT);
        assert!(channels.iter().all(|channel| channel.len() == SAMPLE_COUNT));
        assert_eq!(edges(&channels[10]).len(), 579); // initial + 288 strobes + endpoint pulse
        assert_eq!(
            edges(&channels[8])
                .iter()
                .filter(|sample| !sample.value)
                .count(),
            24
        );
        assert_eq!(
            edges(&channels[10]).last().unwrap().start_time_ns,
            59_999_000_000
        );
    }

    #[test]
    fn packed_and_edge_views_describe_the_same_signal() {
        for values in demo_channels() {
            let block = SampleBlock::new(pack(&values), 0, SAMPLE_COUNT, TIMESTAMP_STEP_NS);
            for sample in edges(&values) {
                let position = sample.start_time_ns / TIMESTAMP_STEP_NS;
                assert_eq!(block.get_bit(position), sample.value);
            }
        }
    }

    #[test]
    fn capture_carries_the_documented_parallel_and_spi_words() {
        let channels = demo_channels();
        let parallel = edges(&channels[10])
            .into_iter()
            .filter(|sample| sample.value)
            .map(|sample| {
                let position = (sample.start_time_ns / TIMESTAMP_STEP_NS) as usize;
                channels
                    .iter()
                    .take(8)
                    .enumerate()
                    .fold(0u8, |word, (bit, channel)| {
                        word | (u8::from(channel[position]) << bit)
                    })
            })
            .collect::<Vec<_>>();
        let cycle_parallel = [
            0x10, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef, 0x55, 0xaa, 0x0f, 0xf0, 0x3c, 0xc3,
            0x5a, 0xa5, 0x01, 0x02, 0x04, 0x08, 0x10, 0x20, 0x40, 0x80,
        ];
        assert_eq!(
            &parallel[..cycle_parallel.len() * CYCLE_COUNT],
            cycle_parallel.repeat(CYCLE_COUNT)
        );

        let spi_words = |data_channel: usize| {
            edges(&channels[7])
                .into_iter()
                .filter(|sample| sample.value)
                .filter(|sample| {
                    let position = (sample.start_time_ns / TIMESTAMP_STEP_NS) as usize;
                    !channels[8][position]
                })
                .map(|sample| {
                    let position = (sample.start_time_ns / TIMESTAMP_STEP_NS) as usize;
                    channels[data_channel][position]
                })
                .collect::<Vec<_>>()
                .as_chunks::<8>().0.iter()
                .map(|bits| {
                    bits.iter()
                        .fold(0u8, |word, bit| (word << 1) | u8::from(*bit))
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(
            spi_words(6),
            [0x9a, 0xbc, 0x42, 0xde, 0xad].repeat(CYCLE_COUNT)
        );
        assert_eq!(
            spi_words(5),
            [0x11, 0x22, 0x33, 0xbe, 0xef].repeat(CYCLE_COUNT)
        );
    }
}
