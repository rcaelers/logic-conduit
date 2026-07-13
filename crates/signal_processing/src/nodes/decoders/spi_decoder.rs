//! SPI decoder — edge-by-edge sequential design
//!
//! Processes SPI signals one edge at a time using [`Receiver`],
//! which provides peek/putback semantics over a crossbeam channel.
//!
//! Flow per transaction:
//!   1. Wait for CS to go active (read only from CS channel)
//!   2. Wait for CS to go inactive → establishes full CS time window
//!   3. Discard CLK/MOSI/MISO edges from before CS active
//!   4. For each CLK sampling edge within the window, read MOSI/MISO value
//!   5. After bits_per_word bits → emit a `Word` on each configured line's
//!      own output port (MOSI and MISO are independent word streams, not
//!      two fields of one event — a consumer that only cares about one
//!      line wires only that port)
//!   6. Continue collecting words until CLK edges pass the CS window
//!
//! Because each data value is obtained by blocking recv (not try_recv),
//! the race condition from the old batch-decode approach is eliminated.

use std::collections::VecDeque;
use std::sync::Arc;

use tracing::{debug, trace};

use super::types::{BitOrder, CsPolarity, SpiMode};
use crate::runtime::Receiver;
use crate::runtime::edge_query::EdgeQuery;
use crate::runtime::events::Word;
use crate::runtime::node::{InputPort, OutputPort, ProcessNode, WorkError, WorkResult};
use crate::runtime::protocol::ProtocolKind;
use crate::runtime::sample::Sample;

/// SPI decoder node
///
/// Inputs: cs, clk, mosi (optional), miso (optional) — Sample channels
/// Outputs: `mosi_words` (if `has_mosi`), `miso_words` (if `has_miso`) —
/// independent `Word` streams, one per configured line.
pub struct SpiDecoder {
    name: String,
    mode: SpiMode,
    bits_per_word: usize,
    has_mosi: bool,
    has_miso: bool,
    cs_polarity: CsPolarity,
    bit_order: BitOrder,

    /// Per-channel putback buffers, persisted across work() calls.
    /// Indexed by CS=0, CLK=1, MOSI=2, MISO=3.
    channel_buffers: Vec<VecDeque<Sample>>,

    /// Tracks CLK state for edge detection across work() boundaries.
    prev_clk: bool,

    /// Transaction counter for logging.
    tx_count: u64,

    /// Query-mode CS search cursor, persisted across work() calls (the
    /// index-query equivalent of the streaming CS `Receiver`'s implicit
    /// position). Unused in streaming mode.
    query_cs_position: u64,
}

impl SpiDecoder {
    /// Create a new SPI decoder with active-low CS (standard)
    pub fn new(mode: SpiMode, bits_per_word: usize, has_mosi: bool, has_miso: bool) -> Self {
        Self::with_cs_polarity(
            mode,
            bits_per_word,
            has_mosi,
            has_miso,
            CsPolarity::ActiveLow,
        )
    }

    /// Create a new SPI decoder with configurable CS polarity
    pub fn with_cs_polarity(
        mode: SpiMode,
        bits_per_word: usize,
        has_mosi: bool,
        has_miso: bool,
        cs_polarity: CsPolarity,
    ) -> Self {
        let num_channels = 2 + usize::from(has_mosi) + usize::from(has_miso);
        Self {
            name: "spi_decoder".to_string(),
            mode,
            bits_per_word,
            has_mosi,
            has_miso,
            cs_polarity,
            bit_order: BitOrder::default(),
            channel_buffers: (0..num_channels).map(|_| VecDeque::new()).collect(),
            prev_clk: false,
            tx_count: 0,
            query_cs_position: 0,
        }
    }

    /// With custom name
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    /// With a bit order other than the default MSB-first
    pub fn with_bit_order(mut self, bit_order: BitOrder) -> Self {
        self.bit_order = bit_order;
        self
    }

    /// Whether the given mode samples on rising CLK edge.
    fn samples_on_rising(&self) -> bool {
        matches!(self.mode, SpiMode::Mode0 | SpiMode::Mode3)
    }

    /// Read the value of a signal channel at a given timestamp.
    ///
    /// With Sample format, an edge is valid from start_time_ns until the
    /// next edge's start_time_ns. We peek at the next edge to determine when
    /// the current edge ends.
    ///
    /// Returns None if the channel is exhausted before finding a valid edge.
    fn value_at_time(
        channel: &mut Receiver<'_, Sample>,
        timestamp: u64,
    ) -> WorkResult<Option<bool>> {
        loop {
            let current = match channel.recv() {
                Ok(edge) => edge,
                Err(WorkError::Shutdown) => {
                    debug!("Channel recv returned Shutdown at timestamp {}", timestamp);
                    return Ok(None);
                }
                Err(e) => return Err(e),
            };

            if current.start_time_ns > timestamp {
                debug!(
                    "value_at_time: edge starts after timestamp ({} > {})",
                    current.start_time_ns, timestamp
                );
            }

            match channel.peek() {
                Ok(next) => {
                    // Check if timestamp is in [current.start_time_ns, next.start_time_ns)
                    if current.start_time_ns <= timestamp && timestamp < next.start_time_ns {
                        channel.put_back(current);
                        return Ok(Some(current.value));
                    }
                    // timestamp >= next.start_time_ns, current has ended - continue
                }
                Err(WorkError::Shutdown) => {
                    // Channel closed - current is the last edge, extends to infinity
                    debug!("Channel peek returned Shutdown at timestamp {}", timestamp);
                    if current.start_time_ns <= timestamp {
                        channel.put_back(current);
                        return Ok(Some(current.value));
                    } else {
                        return Ok(None);
                    }
                }
                Err(e) => return Err(e),
            }
        }
    }
}

impl ProcessNode for SpiDecoder {
    fn name(&self) -> &str {
        &self.name
    }

    fn num_inputs(&self) -> usize {
        2 + usize::from(self.has_mosi) + usize::from(self.has_miso)
    }

    fn num_outputs(&self) -> usize {
        usize::from(self.has_mosi) + usize::from(self.has_miso)
    }

    fn input_schema(&self) -> Vec<crate::runtime::ports::PortSchema> {
        use crate::runtime::ports::{PortDirection, PortSchema};

        // Every input this decoder has is a raw binary channel: prefer
        // skip-ahead queries over streaming every dead-time edge, fall back
        // to streaming for live sources with no index.
        let protocols = vec![ProtocolKind::EdgeQuery, ProtocolKind::Stream];
        let mut schemas = vec![
            PortSchema::new::<Sample>("cs", 0, PortDirection::Input)
                .with_protocols(protocols.clone()),
            PortSchema::new::<Sample>("clk", 1, PortDirection::Input)
                .with_protocols(protocols.clone()),
        ];
        if self.has_mosi {
            schemas.push(
                PortSchema::new::<Sample>("mosi", 2, PortDirection::Input)
                    .with_protocols(protocols.clone()),
            );
        }
        if self.has_miso {
            let idx = 2 + usize::from(self.has_mosi);
            schemas.push(
                PortSchema::new::<Sample>("miso", idx, PortDirection::Input)
                    .with_protocols(protocols.clone()),
            );
        }
        schemas
    }

    fn output_schema(&self) -> Vec<crate::runtime::ports::PortSchema> {
        use crate::runtime::ports::{PortDirection, PortSchema};

        // Mirrors input_schema()'s conditional-port pattern: MOSI's port
        // (if present) always comes before MISO's.
        let mut schemas = Vec::new();
        if self.has_mosi {
            schemas.push(PortSchema::new::<Word>(
                "mosi_words",
                schemas.len(),
                PortDirection::Output,
            ));
        }
        if self.has_miso {
            schemas.push(PortSchema::new::<Word>(
                "miso_words",
                schemas.len(),
                PortDirection::Output,
            ));
        }
        schemas
    }

    fn work(&mut self, inputs: &[InputPort], outputs: &[OutputPort]) -> WorkResult<usize> {
        // Real graphs wire cs/clk/mosi/miso to the same source, so this is
        // an all-or-nothing choice per decoder rather than a per-port mix:
        // if every port this decoder actually uses negotiated EdgeQuery,
        // skip straight to the transactions instead of streaming every
        // sample in between; otherwise stream exactly as before.
        let cs_query = inputs.first().and_then(|p| p.edge_query());
        let clk_query = inputs.get(1).and_then(|p| p.edge_query());
        let mosi_query = if self.has_mosi {
            inputs.get(2).and_then(|p| p.edge_query())
        } else {
            None
        };
        let miso_query = if self.has_miso {
            let idx = 2 + usize::from(self.has_mosi);
            inputs.get(idx).and_then(|p| p.edge_query())
        } else {
            None
        };

        let ready_for_query_mode = cs_query.is_some()
            && clk_query.is_some()
            && (!self.has_mosi || mosi_query.is_some())
            && (!self.has_miso || miso_query.is_some());

        if ready_for_query_mode {
            return self.work_indexed(
                outputs,
                cs_query.expect("checked above"),
                clk_query.expect("checked above"),
                mosi_query,
                miso_query,
            );
        }

        self.work_streamed(inputs, outputs)
    }
}

impl SpiDecoder {
    /// Index-driven path: CS/CLK are located by direct skip-ahead queries
    /// (no streaming, no discarding dead-time edges) and MOSI/MISO are
    /// point-read at each CLK sampling edge. One `work()` call processes
    /// exactly one CS active/inactive transaction window, mirroring
    /// `work_streamed`'s per-call granularity so `self`'s persisted state
    /// (`query_cs_position`, `prev_clk`-equivalent-free since sampling
    /// edges are located directly, `tx_count`) behaves the same way across
    /// repeated scheduler calls.
    fn work_indexed(
        &mut self,
        outputs: &[OutputPort],
        cs_query: Arc<dyn EdgeQuery>,
        clk_query: Arc<dyn EdgeQuery>,
        mosi_query: Option<Arc<dyn EdgeQuery>>,
        miso_query: Option<Arc<dyn EdgeQuery>>,
    ) -> WorkResult<usize> {
        let cs_polarity = self.cs_polarity;
        // Edges alternate, so "wait for active" only ever needs the value
        // that counts as active/inactive, not a predicate — matching
        // `cs_is_active` in `work_streamed` for the two polarities that
        // are actually used in practice. `Disabled` inherits the same
        // degenerate behavior `work_streamed` already has (CS is a
        // mandatory input on this decoder; a polarity that never
        // recognizes "inactive" was already a pre-existing corner case).
        let (active_value, inactive_value) = match cs_polarity {
            CsPolarity::ActiveLow => (false, true),
            CsPolarity::ActiveHigh | CsPolarity::Disabled => (true, false),
        };
        let sample_on_rising = self.samples_on_rising();
        let bits_per_word = self.bits_per_word;
        let bit_order = self.bit_order;
        let bit_position = |bit_index: usize| -> u32 {
            match bit_order {
                BitOrder::MsbFirst => (bits_per_word - 1 - bit_index) as u32,
                BitOrder::LsbFirst => bit_index as u32,
            }
        };
        // A rising-sampling mode looks for the edge landing on `true`
        // (i.e. the rising transition); falling-sampling looks for `false`.
        let sampling_value = sample_on_rising;

        // Output port layout mirrors input_schema(): MOSI's port (if
        // present) always comes before MISO's.
        let mosi_output = if self.has_mosi {
            Some(
                outputs
                    .first()
                    .and_then(|p| p.get::<Word>())
                    .ok_or_else(|| WorkError::NodeError("Missing MOSI output".into()))?,
            )
        } else {
            None
        };
        let miso_output = if self.has_miso {
            let idx = usize::from(self.has_mosi);
            Some(
                outputs
                    .get(idx)
                    .and_then(|p| p.get::<Word>())
                    .ok_or_else(|| WorkError::NodeError("Missing MISO output".into()))?,
            )
        } else {
            None
        };

        let total_samples = cs_query.total_samples();
        let timestamp_step = (1_000_000_000.0 / cs_query.samplerate_hz()) as u64;
        let position_to_ns = |position: u64| position.saturating_mul(timestamp_step);
        // EdgeQuery methods return crate::Result, not WorkResult.
        let query_err = |e: crate::Error| WorkError::NodeError(e.to_string());

        if self.query_cs_position >= total_samples {
            return Err(WorkError::Shutdown);
        }

        // ── 1. Wait for CS to go active ──────────────────────────────────
        debug!("Waiting for CS active (query mode)...");
        let cs_active_start = cs_query
            .next_edge_with_value(self.query_cs_position, active_value, total_samples)
            .map_err(query_err)?
            .ok_or(WorkError::Shutdown)?
            .sample;

        // ── 2. Get CS inactive edge to know the full CS window ───────────
        let cs_inactive_time = cs_query
            .next_edge_with_value(cs_active_start, inactive_value, total_samples)
            .map_err(query_err)?
            .ok_or(WorkError::Shutdown)?
            .sample;

        self.query_cs_position = cs_inactive_time;

        debug!(
            "CS window: {:.9}s — {:.9}s ({:.3}µs)",
            position_to_ns(cs_active_start) as f64 / 1_000_000_000.0,
            position_to_ns(cs_inactive_time) as f64 / 1_000_000_000.0,
            (position_to_ns(cs_inactive_time) - position_to_ns(cs_active_start)) as f64 / 1_000.0,
        );

        // ── 3. No drain needed — CLK/MOSI/MISO were never streamed ───────

        // ── 4. Collect words from CLK within the CS window ───────────────
        let mut words_emitted: usize = 0;
        let mut clk_position = cs_active_start;

        'word_loop: loop {
            let mut mosi_word: u64 = 0;
            let mut miso_word: u64 = 0;
            let mut bits_collected: usize = 0;
            let mut first_clock_edge: Option<u64> = None;

            loop {
                let Some(edge) = clk_query
                    .next_edge_with_value(clk_position, sampling_value, cs_inactive_time)
                    .map_err(query_err)?
                else {
                    if bits_collected > 0 && bits_collected < bits_per_word {
                        debug!("Incomplete word: {}/{} bits", bits_collected, bits_per_word);
                    }
                    break 'word_loop;
                };
                clk_position = edge.sample;

                if first_clock_edge.is_none() {
                    first_clock_edge = Some(edge.sample);
                }

                let sample_position = edge.sample.saturating_sub(1);
                if let Some(ref q) = mosi_query {
                    let mosi_val = q.value_at(sample_position).map_err(query_err)?;
                    if mosi_val {
                        mosi_word |= 1 << bit_position(bits_collected);
                    }
                    trace!(
                        "bit {}: CLK edge at {:.9}s, MOSI={}",
                        bits_collected,
                        position_to_ns(edge.sample) as f64 / 1_000_000_000.0,
                        mosi_val,
                    );
                }
                if let Some(ref q) = miso_query {
                    let miso_val = q.value_at(sample_position).map_err(query_err)?;
                    if miso_val {
                        miso_word |= 1 << bit_position(bits_collected);
                    }
                }

                bits_collected += 1;
                if bits_collected >= bits_per_word {
                    break;
                }
            }

            if bits_collected == bits_per_word {
                let timestamp = first_clock_edge
                    .map(position_to_ns)
                    .unwrap_or_else(|| position_to_ns(cs_active_start));
                // `clk_position` is the word's last sampling edge — the
                // word's real extent, first to last edge.
                let duration = position_to_ns(clk_position).saturating_sub(timestamp);

                words_emitted += 1;
                debug!(
                    "#{}: mosi=0x{:06X} miso=0x{:06X} at {:.9}s",
                    self.tx_count + words_emitted as u64,
                    mosi_word,
                    miso_word,
                    timestamp as f64 / 1_000_000_000.0
                );
                if let Some(ref output) = mosi_output {
                    output.send(Word::spanning(mosi_word, timestamp, duration))?;
                }
                if let Some(ref output) = miso_output {
                    output.send(Word::spanning(miso_word, timestamp, duration))?;
                }
            }
        }

        self.tx_count += words_emitted as u64;
        Ok(words_emitted)
    }

    /// Streaming path: unchanged behavior for live sources or any
    /// connection that didn't negotiate `EdgeQuery`.
    fn work_streamed(&mut self, inputs: &[InputPort], outputs: &[OutputPort]) -> WorkResult<usize> {
        // Extract config values before borrowing channel_buffers
        let cs_polarity = self.cs_polarity;
        let cs_is_active = |value: bool| -> bool {
            match cs_polarity {
                CsPolarity::ActiveLow => !value,
                CsPolarity::ActiveHigh => value,
                CsPolarity::Disabled => true,
            }
        };
        let sample_on_rising = self.samples_on_rising();
        let bits_per_word = self.bits_per_word;
        let has_mosi = self.has_mosi;
        let has_miso = self.has_miso;
        let bit_order = self.bit_order;
        let bit_position = |bit_index: usize| -> u32 {
            match bit_order {
                BitOrder::MsbFirst => (bits_per_word - 1 - bit_index) as u32,
                BitOrder::LsbFirst => bit_index as u32,
            }
        };
        let mut prev_clk = self.prev_clk;

        // ── Create named Receivers per channel with automatic watchdog ───────
        let mut buf_iter = self.channel_buffers.iter_mut();
        let mut cs = inputs
            .first()
            .and_then(|p| p.get::<Sample>(buf_iter.next().unwrap()))
            .ok_or_else(|| WorkError::NodeError("Missing CS input".into()))?;
        let mut clk = inputs
            .get(1)
            .and_then(|p| p.get::<Sample>(buf_iter.next().unwrap()))
            .ok_or_else(|| WorkError::NodeError("Missing CLK input".into()))?;
        let mut mosi = if has_mosi {
            Some(
                inputs
                    .get(2)
                    .and_then(|p| p.get::<Sample>(buf_iter.next().unwrap()))
                    .ok_or_else(|| WorkError::NodeError("Missing MOSI input".into()))?,
            )
        } else {
            None
        };
        let mut miso = if has_miso {
            let idx = 2 + usize::from(has_mosi);
            Some(
                inputs
                    .get(idx)
                    .and_then(|p| p.get::<Sample>(buf_iter.next().unwrap()))
                    .ok_or_else(|| WorkError::NodeError("Missing MISO input".into()))?,
            )
        } else {
            None
        };
        // Output port layout mirrors input_schema(): MOSI's port (if
        // present) always comes before MISO's.
        let mosi_output = if has_mosi {
            Some(
                outputs
                    .first()
                    .and_then(|p| p.get::<Word>())
                    .ok_or_else(|| WorkError::NodeError("Missing MOSI output".into()))?,
            )
        } else {
            None
        };
        let miso_output = if has_miso {
            let idx = usize::from(has_mosi);
            Some(
                outputs
                    .get(idx)
                    .and_then(|p| p.get::<Word>())
                    .ok_or_else(|| WorkError::NodeError("Missing MISO output".into()))?,
            )
        } else {
            None
        };

        // ── 1. Wait for CS to go active ──────────────────────────────────
        // Only read from CS — leave CLK/MOSI/MISO untouched so their edges
        // (which may span the CS-active window) aren't lost.
        debug!("Waiting for CS active...");
        let cs_active_edge = loop {
            let edge = cs.recv()?;
            if cs_is_active(edge.value) {
                break edge;
            }
        };

        let cs_active_start = cs_active_edge.start_time_ns;

        // ── 2. Get CS inactive edge to know the full CS window ───────────
        let cs_inactive_edge = loop {
            let edge = cs.recv()?;
            if !cs_is_active(edge.value) {
                break edge;
            }
        };
        let cs_inactive_time = cs_inactive_edge.start_time_ns;

        debug!(
            "CS window: {:.9}s — {:.9}s ({:.3}µs)",
            cs_active_start as f64 / 1_000_000_000.0,
            cs_inactive_time as f64 / 1_000_000_000.0,
            (cs_inactive_time - cs_active_start) as f64 / 1_000.0,
        );

        // ── 3. Discard CLK/MOSI/MISO edges from before CS active ────────
        clk.drain_before(cs_active_start, |e| e.start_time_ns)?;
        if let Some(ref mut m) = mosi {
            m.drain_before(cs_active_start, |e| e.start_time_ns)?;
        }
        if let Some(ref mut m) = miso {
            m.drain_before(cs_active_start, |e| e.start_time_ns)?;
        }

        // ── 4. Collect words from CLK within the CS window ───────────────
        let mut words_emitted: usize = 0;

        'word_loop: loop {
            let mut mosi_word: u64 = 0;
            let mut miso_word: u64 = 0;
            let mut bits_collected: usize = 0;
            // The word's (first, last) sampling-edge timestamps so far.
            let mut clock_edge_span: Option<(u64, u64)> = None;

            // Collect bits_per_word bits from CLK sampling edges.
            // MOSI/MISO are read on-demand via value_at_time when a
            // CLK sampling edge arrives. CS is already fully consumed;
            // we use cs_inactive_time for bounds.
            loop {
                let edge = clk.recv()?;

                // CLK edge past CS window → transaction is over
                if edge.start_time_ns >= cs_inactive_time {
                    clk.put_back(edge);

                    if bits_collected > 0 && bits_collected < bits_per_word {
                        debug!("Incomplete word: {}/{} bits", bits_collected, bits_per_word);
                    }

                    break 'word_loop;
                }

                // Check for sampling edge
                let is_rising = !prev_clk && edge.value;
                let is_falling = prev_clk && !edge.value;
                prev_clk = edge.value;

                let is_sampling_edge = if sample_on_rising {
                    is_rising
                } else {
                    is_falling
                };

                if !is_sampling_edge {
                    continue;
                }

                // Record first/last clock edge timestamps
                clock_edge_span = Some(match clock_edge_span {
                    None => (edge.start_time_ns, edge.start_time_ns),
                    Some((first, _)) => (first, edge.start_time_ns),
                });

                // Sample data lines at CLK edge time
                let sample_time = edge.start_time_ns.saturating_sub(1);
                if has_mosi {
                    match Self::value_at_time(mosi.as_mut().unwrap(), sample_time)? {
                        Some(mosi_val) => {
                            if mosi_val {
                                mosi_word |= 1 << bit_position(bits_collected);
                            }
                            trace!(
                                "bit {}: CLK edge at {:.9}s, MOSI={}",
                                bits_collected,
                                edge.start_time_ns as f64 / 1_000_000_000.0,
                                mosi_val,
                            );
                        }
                        None => {
                            // MOSI channel exhausted - signal shutdown
                            debug!("MOSI channel exhausted, shutting down decoder");
                            return Err(WorkError::Shutdown);
                        }
                    }
                }

                if has_miso {
                    match Self::value_at_time(miso.as_mut().unwrap(), sample_time)? {
                        Some(miso_val) => {
                            if miso_val {
                                miso_word |= 1 << bit_position(bits_collected);
                            }
                        }
                        None => {
                            // MISO channel exhausted - signal shutdown
                            debug!("MISO channel exhausted, shutting down decoder");
                            return Err(WorkError::Shutdown);
                        }
                    }
                }

                bits_collected += 1;
                if bits_collected >= bits_per_word {
                    break;
                }
            }

            // We have a complete word
            if bits_collected == bits_per_word {
                // First to last sampling edge — the word's real extent.
                let (timestamp, last_edge) =
                    clock_edge_span.unwrap_or((cs_active_start, cs_active_start));
                let duration = last_edge.saturating_sub(timestamp);

                words_emitted += 1;
                debug!(
                    "#{}: mosi=0x{:06X} miso=0x{:06X} at {:.9}s",
                    self.tx_count + words_emitted as u64,
                    mosi_word,
                    miso_word,
                    timestamp as f64 / 1_000_000_000.0
                );
                if let Some(ref output) = mosi_output {
                    output.send(Word::spanning(mosi_word, timestamp, duration))?;
                }
                if let Some(ref output) = miso_output {
                    output.send(Word::spanning(miso_word, timestamp, duration))?;
                }
            }
        }

        // Write back mutable state
        self.prev_clk = prev_clk;
        self.tx_count += words_emitted as u64;

        Ok(words_emitted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_decoder_creation() {
        let decoder = SpiDecoder::new(SpiMode::Mode0, 8, true, false);
        assert_eq!(decoder.bits_per_word, 8);
        assert!(decoder.has_mosi);
        assert!(!decoder.has_miso);
        assert_eq!(decoder.channel_buffers.len(), 3); // CS, CLK, MOSI
    }

    #[test]
    fn test_decoder_creation_with_miso() {
        let decoder = SpiDecoder::new(SpiMode::Mode0, 24, true, true);
        assert_eq!(decoder.bits_per_word, 24);
        assert!(decoder.has_mosi);
        assert!(decoder.has_miso);
        assert_eq!(decoder.channel_buffers.len(), 4); // CS, CLK, MOSI, MISO
    }

    #[test]
    fn test_cs_polarity() {
        let decoder_low = SpiDecoder::new(SpiMode::Mode0, 8, true, false);
        assert_eq!(decoder_low.cs_polarity, CsPolarity::ActiveLow);

        let decoder_high =
            SpiDecoder::with_cs_polarity(SpiMode::Mode0, 8, true, false, CsPolarity::ActiveHigh);
        assert_eq!(decoder_high.cs_polarity, CsPolarity::ActiveHigh);
    }

    /// MOSI and MISO are independent `Word` streams on independent output
    /// ports — the regression test for the bug this design fixes (the old
    /// single-`SpiTransfer`-port design could never actually deliver MISO
    /// to a consumer; see `docs/PIPELINE_DESIGN.md` and this file's
    /// module doc). 4-bit words, MSB-first: MOSI = 0b1010, MISO = 0b0101,
    /// sampled on CLK's rising edge (Mode0).
    #[test]
    fn work_streamed_emits_independent_mosi_and_miso_word_streams() {
        use crossbeam_channel::bounded;

        use crate::runtime::sender::{ChannelMessage, Sender};
        use crate::runtime::watchdog::Watchdog;

        let wd = Watchdog::new();

        let cs_samples = [Sample::new(false, 0), Sample::new(true, 1000)];
        let clk_samples = [
            Sample::new(false, 0),
            Sample::new(true, 100),
            Sample::new(false, 200),
            Sample::new(true, 300),
            Sample::new(false, 400),
            Sample::new(true, 500),
            Sample::new(false, 600),
            Sample::new(true, 700),
        ];
        // Bit i is read 1ns before CLK's i-th rising edge.
        let mosi_samples = [
            Sample::new(true, 0),    // bit0 = 1
            Sample::new(false, 200), // bit1 = 0
            Sample::new(true, 400),  // bit2 = 1
            Sample::new(false, 600), // bit3 = 0
        ];
        let miso_samples = [
            Sample::new(false, 0),   // bit0 = 0
            Sample::new(true, 200),  // bit1 = 1
            Sample::new(false, 400), // bit2 = 0
            Sample::new(true, 600),  // bit3 = 1
        ];

        let make_input = |samples: &[Sample], port: &str| {
            let (tx, rx) = bounded::<ChannelMessage<Sample>>(samples.len() + 1);
            for &s in samples {
                tx.send(ChannelMessage::Sample(s)).unwrap();
            }
            drop(tx);
            InputPort::new_with_watchdog(rx, &wd, "spi", port)
        };
        let inputs = [
            make_input(&cs_samples, "cs"),
            make_input(&clk_samples, "clk"),
            make_input(&mosi_samples, "mosi"),
            make_input(&miso_samples, "miso"),
        ];

        let (mosi_tx, mosi_rx) = bounded::<ChannelMessage<Word>>(16);
        let (miso_tx, miso_rx) = bounded::<ChannelMessage<Word>>(16);
        let outputs = [
            OutputPort::new_with_watchdog(Sender::new(vec![mosi_tx]), &wd, "spi", "mosi_words"),
            OutputPort::new_with_watchdog(Sender::new(vec![miso_tx]), &wd, "spi", "miso_words"),
        ];

        let mut decoder = SpiDecoder::new(SpiMode::Mode0, 4, true, true);
        loop {
            match decoder.work(&inputs, &outputs) {
                Ok(_) => {}
                Err(WorkError::Shutdown) => break,
                Err(e) => panic!("unexpected error: {e}"),
            }
        }

        let collect = |rx: crossbeam_channel::Receiver<ChannelMessage<Word>>| -> Vec<Word> {
            rx.try_iter()
                .filter_map(|m| match m {
                    ChannelMessage::Sample(w) => Some(w),
                    _ => None,
                })
                .collect()
        };
        // The word spans its sampling edges: first at 100ns, last at 700ns.
        assert_eq!(collect(mosi_rx), vec![Word::spanning(0b1010, 100, 600)]);
        assert_eq!(collect(miso_rx), vec![Word::spanning(0b0101, 100, 600)]);
    }

    // ── Differential test: query-mode output must match streaming-mode ──

    /// Wraps a node and forces its outputs onto the `Stream` protocol
    /// regardless of what the wrapped node would otherwise prefer. Used to
    /// get a guaranteed streaming baseline from `DslFileSource` (which
    /// prefers `EdgeQuery`) to differential-test against.
    struct ForceStreamOutput<N>(N);

    impl<N: ProcessNode> ProcessNode for ForceStreamOutput<N> {
        fn name(&self) -> &str {
            self.0.name()
        }
        fn should_stop(&self) -> bool {
            self.0.should_stop()
        }
        fn is_self_threading(&self) -> bool {
            self.0.is_self_threading()
        }
        fn num_inputs(&self) -> usize {
            self.0.num_inputs()
        }
        fn num_outputs(&self) -> usize {
            self.0.num_outputs()
        }
        fn input_schema(&self) -> Vec<crate::runtime::ports::PortSchema> {
            self.0.input_schema()
        }
        fn output_schema(&self) -> Vec<crate::runtime::ports::PortSchema> {
            self.0
                .output_schema()
                .into_iter()
                .map(|schema| schema.with_protocols(vec![ProtocolKind::Stream]))
                .collect()
        }
        fn node_type(&self) -> &str {
            self.0.node_type()
        }
        fn work(&mut self, inputs: &[InputPort], outputs: &[OutputPort]) -> WorkResult<usize> {
            self.0.work(inputs, outputs)
        }
    }

    /// Test-only sink that collects everything sent to its single input.
    struct CollectWords {
        collected: std::sync::Arc<std::sync::Mutex<Vec<Word>>>,
        buffer: VecDeque<Word>,
    }

    impl ProcessNode for CollectWords {
        fn name(&self) -> &str {
            "collect"
        }
        fn num_inputs(&self) -> usize {
            1
        }
        fn num_outputs(&self) -> usize {
            0
        }
        fn input_schema(&self) -> Vec<crate::runtime::ports::PortSchema> {
            use crate::runtime::ports::{PortDirection, PortSchema};
            vec![PortSchema::new::<Word>("data", 0, PortDirection::Input)]
        }
        fn work(&mut self, inputs: &[InputPort], _outputs: &[OutputPort]) -> WorkResult<usize> {
            let mut recv = inputs
                .first()
                .and_then(|p| p.get::<Word>(&mut self.buffer))
                .ok_or_else(|| WorkError::NodeError("Missing collector input".into()))?;
            let item = recv.recv()?;
            self.collected.lock().unwrap().push(item);
            Ok(1)
        }
    }

    /// Runs `_captures/wipneus5.dsl` through a real 3-node pipeline
    /// (`DslFileSource` -> `SpiDecoder` -> collector), bounded to
    /// `max_samples` so the test is fast, and returns the decoded MOSI
    /// words. `force_stream` wraps the source so the connection
    /// negotiates `Stream` instead of the `EdgeQuery` both sides would
    /// otherwise prefer.
    fn decode_wipneus5(path: &std::path::Path, max_samples: u64, force_stream: bool) -> Vec<Word> {
        use crate::DslFileSource;
        use crate::runtime::Pipeline;

        let source = DslFileSource::new(path, 9)
            .expect("wipneus5.dsl should open")
            .with_max_samples(Some(max_samples));
        let decoder = SpiDecoder::new(SpiMode::Mode0, 24, true, false);
        let collected = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));

        let mut pipeline = Pipeline::new();
        if force_stream {
            pipeline
                .add_process("source", ForceStreamOutput(source))
                .unwrap();
        } else {
            pipeline.add_process("source", source).unwrap();
        }
        pipeline.add_process("spi", decoder).unwrap();
        pipeline
            .add_process(
                "collect",
                CollectWords {
                    collected: collected.clone(),
                    buffer: VecDeque::new(),
                },
            )
            .unwrap();

        pipeline.connect("source", "ch7", "spi", "clk").unwrap();
        pipeline.connect("source", "ch8", "spi", "cs").unwrap();
        pipeline.connect("source", "ch6", "spi", "mosi").unwrap();
        pipeline
            .connect("spi", "mosi_words", "collect", "data")
            .unwrap();

        pipeline.build().unwrap().wait();

        std::sync::Arc::try_unwrap(collected)
            .unwrap()
            .into_inner()
            .unwrap()
    }

    #[test]
    fn test_query_mode_matches_streaming_mode() {
        let path = std::path::Path::new("_captures/wipneus5.dsl");
        if !path.exists() {
            return;
        }

        // Bounded prefix: fast to run, still large enough to very likely
        // contain real SPI traffic on this fixture (the same file/channel
        // mapping the golden `run_reference` pipeline in
        // crates/logic_analyzer_graph/src/compiler/mod.rs uses).
        const MAX_SAMPLES: u64 = 200_000_000;

        let streamed = decode_wipneus5(path, MAX_SAMPLES, true);
        let queried = decode_wipneus5(path, MAX_SAMPLES, false);

        assert!(
            !streamed.is_empty(),
            "expected at least one decoded SPI transfer in the first {MAX_SAMPLES} samples \
             to make this comparison meaningful"
        );

        let as_tuple = |w: &Word| (w.value, w.timestamp_ns, w.duration_ns);
        let streamed_view: Vec<_> = streamed.iter().map(as_tuple).collect();
        let queried_view: Vec<_> = queried.iter().map(as_tuple).collect();

        assert_eq!(
            streamed_view, queried_view,
            "query-mode SpiDecoder must produce byte-identical output to the streaming path"
        );
    }
}
