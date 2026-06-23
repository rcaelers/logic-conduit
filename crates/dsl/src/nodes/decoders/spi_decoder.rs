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
//!   5. After bits_per_word bits → emit SpiTransfer
//!   6. Continue collecting words until CLK edges pass the CS window
//!
//! Because each data value is obtained by blocking recv (not try_recv),
//! the race condition from the old batch-decode approach is eliminated.

use super::types::{CsPolarity, SpiMode, SpiTransfer, TimingInfo};
use crate::runtime::Receiver;
use crate::runtime::node::{InputPort, OutputPort, ProcessNode, WorkError, WorkResult};
use crate::runtime::sample::Sample;
use std::collections::VecDeque;
use tracing::{debug, trace};

/// SPI decoder node
///
/// Inputs: cs, clk, mosi (optional), miso (optional) — Sample channels
/// Output: SpiTransfer events
pub struct SpiDecoder {
    name: String,
    mode: SpiMode,
    bits_per_word: usize,
    has_mosi: bool,
    has_miso: bool,
    cs_polarity: CsPolarity,

    /// Per-channel putback buffers, persisted across work() calls.
    /// Indexed by CS=0, CLK=1, MOSI=2, MISO=3.
    channel_buffers: Vec<VecDeque<Sample>>,

    /// Tracks CLK state for edge detection across work() boundaries.
    prev_clk: bool,

    /// Transaction counter for logging.
    tx_count: u64,
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
            channel_buffers: (0..num_channels).map(|_| VecDeque::new()).collect(),
            prev_clk: false,
            tx_count: 0,
        }
    }

    /// With custom name
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    /// Whether the given mode samples on rising CLK edge.
    fn samples_on_rising(&self) -> bool {
        matches!(self.mode, SpiMode::Mode0 | SpiMode::Mode3)
    }

    /// Read the value of a signal channel at a given timestamp.
    ///
    /// With Sample format, an edge is valid from start_time until the
    /// next edge's start_time. We peek at the next edge to determine when
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

            if current.start_time > timestamp {
                debug!(
                    "value_at_time: edge starts after timestamp ({} > {})",
                    current.start_time, timestamp
                );
            }

            match channel.peek() {
                Ok(next) => {
                    // Check if timestamp is in [current.start_time, next.start_time)
                    if current.start_time <= timestamp && timestamp < next.start_time {
                        channel.put_back(current);
                        return Ok(Some(current.value));
                    }
                    // timestamp >= next.start_time, current has ended - continue
                }
                Err(WorkError::Shutdown) => {
                    // Channel closed - current is the last edge, extends to infinity
                    debug!("Channel peek returned Shutdown at timestamp {}", timestamp);
                    if current.start_time <= timestamp {
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
        1
    }

    fn input_schema(&self) -> Vec<crate::runtime::ports::PortSchema> {
        use crate::runtime::ports::{PortDirection, PortSchema};

        let mut schemas = vec![
            PortSchema::new::<Sample>("cs", 0, PortDirection::Input),
            PortSchema::new::<Sample>("clk", 1, PortDirection::Input),
        ];
        if self.has_mosi {
            schemas.push(PortSchema::new::<Sample>("mosi", 2, PortDirection::Input));
        }
        if self.has_miso {
            let idx = 2 + usize::from(self.has_mosi);
            schemas.push(PortSchema::new::<Sample>("miso", idx, PortDirection::Input));
        }
        schemas
    }

    fn output_schema(&self) -> Vec<crate::runtime::ports::PortSchema> {
        use crate::runtime::ports::{PortDirection, PortSchema};
        vec![PortSchema::new::<SpiTransfer>(
            "spi_transfers",
            0,
            PortDirection::Output,
        )]
    }

    fn work(&mut self, inputs: &[InputPort], outputs: &[OutputPort]) -> WorkResult<usize> {
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
        let output = outputs
            .first()
            .and_then(|p| p.get::<SpiTransfer>())
            .ok_or_else(|| WorkError::NodeError("Missing output".into()))?;

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

        let cs_active_start = cs_active_edge.start_time;

        // ── 2. Get CS inactive edge to know the full CS window ───────────
        let cs_inactive_edge = loop {
            let edge = cs.recv()?;
            if !cs_is_active(edge.value) {
                break edge;
            }
        };
        let cs_inactive_time = cs_inactive_edge.start_time;

        debug!(
            "CS window: {:.9}s — {:.9}s ({:.3}µs)",
            cs_active_start as f64 / 1_000_000_000.0,
            cs_inactive_time as f64 / 1_000_000_000.0,
            (cs_inactive_time - cs_active_start) as f64 / 1_000.0,
        );

        // ── 3. Discard CLK/MOSI/MISO edges from before CS active ────────
        clk.drain_before(cs_active_start, |e| e.start_time)?;
        if let Some(ref mut m) = mosi {
            m.drain_before(cs_active_start, |e| e.start_time)?;
        }
        if let Some(ref mut m) = miso {
            m.drain_before(cs_active_start, |e| e.start_time)?;
        }

        // ── 4. Collect words from CLK within the CS window ───────────────
        let mut words_emitted: usize = 0;

        'word_loop: loop {
            let mut mosi_word: u32 = 0;
            let mut miso_word: u32 = 0;
            let mut bits_collected: usize = 0;
            let mut first_clock_edge: Option<u64> = None;

            // Collect bits_per_word bits from CLK sampling edges.
            // MOSI/MISO are read on-demand via value_at_time when a
            // CLK sampling edge arrives. CS is already fully consumed;
            // we use cs_inactive_time for bounds.
            loop {
                let edge = clk.recv()?;

                // CLK edge past CS window → transaction is over
                if edge.start_time >= cs_inactive_time {
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

                // Record first clock edge timestamp
                if first_clock_edge.is_none() {
                    first_clock_edge = Some(edge.start_time);
                }

                // Sample data lines at CLK edge time
                let sample_time = edge.start_time.saturating_sub(1);
                if has_mosi {
                    match Self::value_at_time(mosi.as_mut().unwrap(), sample_time)? {
                        Some(mosi_val) => {
                            if mosi_val {
                                mosi_word |= 1 << (bits_per_word - 1 - bits_collected);
                            }
                            trace!(
                                "bit {}: CLK edge at {:.9}s, MOSI={}",
                                bits_collected,
                                edge.start_time as f64 / 1_000_000_000.0,
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
                                miso_word |= 1 << (bits_per_word - 1 - bits_collected);
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
                let timestamp = first_clock_edge.unwrap_or(cs_active_start);
                let transfer = SpiTransfer {
                    mosi: mosi_word,
                    miso: miso_word,
                    timing: TimingInfo::new(timestamp as f64 / 1_000.0, timestamp),
                };

                words_emitted += 1;
                debug!(
                    "#{}: 0x{:06X} at {:.9}s",
                    self.tx_count + words_emitted as u64,
                    transfer.mosi,
                    timestamp as f64 / 1_000_000_000.0
                );
                output.send(transfer)?;
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
}
