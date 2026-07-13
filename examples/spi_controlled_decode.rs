//! Example: SPI-controlled parallel bus decoding
//!
//! Demonstrates using SPI commands to enable/disable a parallel bus decoder.
//! This is useful when a device uses SPI commands to control when valid data
//! appears on a parallel bus.
//!
//! **Enable Logic**: The parallel decoder is enabled when:
//! - The SPI command controller output (enable_signal) is TRUE, AND
//! - The CS (chip select) signal is INACTIVE (high for active-low CS)
//!
//! When both conditions are met and a strobe trigger occurs, the parallel data
//! is decoded and written to a binary file. When either condition becomes false,
//! decoding stops (but the file remains open until explicitly disabled).
//!
//! When the SPI disable command is received, the current capture file is closed.
//! When the enable command is received, a new binary file will be created on the
//! first decoded word.
//!
//! Binary format: Raw byte stream, one byte per parallel word (value as u8).
//!
//! Index file (captures.csv): Contains metadata for all binary files:
//!   - file_num: Sequential file number
//!   - filename: Binary file name (e.g., capture_0001.bin)
//!   - bytes: File size in bytes (1 byte per word)
//!   - start_time_us: Timestamp of first word (microseconds)
//!   - end_time_us: Timestamp of last word (microseconds)
//!   - duration_us: Time span covered by the file (microseconds)
//!   - start_pos: Sample position of first word
//!   - end_pos: Sample position of last word
//!
//! Usage:
//!   cargo run --release --example spi_controlled_decode -- \
//!       --file scan.dsl \
//!       --spi-cs 8 --spi-clk 7 --spi-mosi 6 \
//!       --parallel-strobe 10 --parallel-data 0 1 2 3 4 5 6 7 \
//!       --enable-cmd 0x600081 --disable-cmd 0x600000 \
//!       -n 100 \
//!       --output-dir captures

use std::collections::VecDeque;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::PathBuf;

use clap::Parser;
use crossbeam_channel::TryRecvError;
use tracing::{debug, info};

use signal_processing::nodes::decoders::{
    CsPolarity, ParallelDecoder, SpiDecoder, SpiMode, StrobeMode,
};
use signal_processing::{
    DslFileSource, InputPort, OutputPort, Pipeline, PortDirection, PortSchema, ProcessNode, Sample,
    Word, WorkError, WorkResult,
};

/// One complete TGCK cycle record for the CSV output.
struct TgckRecord {
    rising_byte_index: usize,
    rising_timestamp: u64,
    falling_byte_index: usize,
    falling_timestamp: u64,
    first_clock_after_rising_byte_index: usize,
    first_clock_after_rising_timestamp: u64,
    first_clock_after_falling_byte_index: usize,
    first_clock_after_falling_timestamp: u64,
}

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Path to DSL file
    #[arg(short, long)]
    file: String,

    /// SPI chip select channel
    #[arg(long)]
    spi_cs: usize,

    /// SPI clock channel
    #[arg(long)]
    spi_clk: usize,

    /// SPI MOSI channel
    #[arg(long)]
    spi_mosi: usize,

    /// Parallel strobe channel
    #[arg(long)]
    parallel_strobe: usize,

    /// Parallel data channels (in order)
    #[arg(long, num_args = 1..)]
    parallel_data: Vec<usize>,

    /// SPI command that enables parallel decoder (hex)
    #[arg(long, value_parser = parse_hex)]
    enable_cmd: u64,

    /// SPI command that disables parallel decoder (hex)
    #[arg(long, value_parser = parse_hex)]
    disable_cmd: u64,

    /// TGCK channel number (rising edges mark line boundaries)
    #[arg(long, default_value_t = 9)]
    tgck: usize,

    /// Output directory for captured data files
    #[arg(short, long, default_value = "output")]
    output_dir: PathBuf,
}

fn parse_hex(s: &str) -> Result<u64, std::num::ParseIntError> {
    let s = s.trim_start_matches("0x").trim_start_matches("0X");
    u64::from_str_radix(s, 16)
}

/// Controls a boolean state based on SPI command values
///
/// This node watches SPI transfers and emits state changes when specific
/// command values are detected. Used to enable/disable downstream nodes.
///
/// Outputs Sample with start_time_ns = position for instantaneous state changes.
struct SpiCommandController {
    name: String,
    enable_command: u64,
    disable_command: u64,
    current_state: bool,
    initial_state_sent: bool,
    tx_count: u64,
}

impl SpiCommandController {
    fn new(enable_command: u64, disable_command: u64) -> Self {
        Self {
            name: "spi_command_controller".to_string(),
            enable_command,
            disable_command,
            current_state: false,
            initial_state_sent: false,
            tx_count: 0,
        }
    }
}

impl ProcessNode for SpiCommandController {
    fn name(&self) -> &str {
        &self.name
    }

    fn num_inputs(&self) -> usize {
        1 // Word input
    }

    fn num_outputs(&self) -> usize {
        1 // Sample output
    }

    fn input_schema(&self) -> Vec<PortSchema> {
        vec![PortSchema::new::<Word>("spi_in", 0, PortDirection::Input)]
    }

    fn output_schema(&self) -> Vec<PortSchema> {
        vec![PortSchema::new::<Sample>(
            "enable_signal",
            0,
            PortDirection::Output,
        )]
    }

    fn work(&mut self, inputs: &[InputPort], outputs: &[OutputPort]) -> WorkResult<usize> {
        // Expect 1 input (Word) and 1 output (Sample)
        let mut input_buffer = VecDeque::new();
        let mut input = inputs
            .first()
            .and_then(|port| port.get::<Word>(&mut input_buffer))
            .ok_or_else(|| WorkError::NodeError("Missing input channel".to_string()))?;

        let output = outputs
            .first()
            .and_then(|port| port.get::<Sample>())
            .ok_or_else(|| WorkError::NodeError("Missing output channel".to_string()))?;

        // Send initial state on first call to avoid deadlock
        if !self.initial_state_sent {
            debug!(
                "[SpiCommandController] Sending initial state: {}",
                self.current_state
            );
            let sample = Sample::new(self.current_state, 0);
            output.send(sample)?;
            self.initial_state_sent = true;
        }

        // Process one SPI transfer
        let transfer = input.recv()?;
        let mosi_value = transfer.value;

        debug!(
            "[SpiCommandController] command: #{} 0x{:06X}",
            self.tx_count, mosi_value
        );

        let new_state = if mosi_value == self.enable_command {
            true
        } else if mosi_value == self.disable_command {
            false
        } else {
            self.current_state
        };

        self.tx_count += 1;

        // Only emit if state changed
        if new_state != self.current_state {
            debug!(
                "[SpiCommandController] State change: {} -> {} (command: 0x{:06X})",
                self.current_state, new_state, mosi_value
            );
            self.current_state = new_state;
            let timestamp = transfer.timestamp_ns;
            let sample = Sample::new(self.current_state, timestamp);
            output.send(sample)?;
        }

        Ok(1)
    }
}

/// Sink that writes parallel words to files, creating a new file on each enable
struct ControlledParallelWriter {
    output_dir: PathBuf,
    count: usize,
    current_file: Option<BufWriter<File>>,
    file_count: usize,
    words_in_file: usize,
    // Persistent buffer for enable channel (for peek/putback)
    enable_buffer: VecDeque<Sample>,
    // Current enable state (updated from edges as they arrive)
    current_enable_state: bool,
    current_enable_timestamp: u64,
    // Metadata for current file
    current_file_start_time_ns: Option<u64>,
    current_file_end_time_ns: Option<u64>,
    current_file_start_pos: Option<u64>,
    current_file_end_pos: Option<u64>,
    // TGCK tracking
    tgck_buffer: VecDeque<Sample>,
    last_tgck_value: bool,
    tgck_records: Vec<TgckRecord>,
    // State for building current TGCK record
    tgck_current_rising: Option<(usize, u64)>,
    tgck_current_falling: Option<(usize, u64)>,
    tgck_first_clock_after_rising: Option<(usize, u64)>,
    tgck_first_clock_after_falling: Option<(usize, u64)>,
    tgck_need_clock_after_rising: bool,
    tgck_need_clock_after_falling: bool,
    // Channel closed tracking
    enable_channel_closed: bool,
    tgck_channel_closed: bool,
    // Reusable buffers to avoid allocations
    edges_to_process_buf: Vec<Sample>,
    tgck_rising_buf: Vec<u64>,
    tgck_falling_buf: Vec<u64>,
}

impl ControlledParallelWriter {
    fn new(output_dir: PathBuf) -> Self {
        Self {
            output_dir,
            count: 0,
            current_file: None,
            file_count: 0,
            words_in_file: 0,
            enable_buffer: VecDeque::new(),
            current_enable_state: false,
            current_enable_timestamp: 0,
            current_file_start_time_ns: None,
            current_file_end_time_ns: None,
            current_file_start_pos: None,
            current_file_end_pos: None,
            tgck_buffer: VecDeque::new(),
            last_tgck_value: false,
            tgck_records: Vec::new(),
            tgck_current_rising: None,
            tgck_current_falling: None,
            tgck_first_clock_after_rising: None,
            tgck_first_clock_after_falling: None,
            tgck_need_clock_after_rising: false,
            tgck_need_clock_after_falling: false,
            enable_channel_closed: false,
            tgck_channel_closed: false,
            edges_to_process_buf: Vec::with_capacity(100),
            tgck_rising_buf: Vec::with_capacity(1000),
            tgck_falling_buf: Vec::with_capacity(1000),
        }
    }

    /// Compute capture width (bytes per line) from TGCK falling_byte_index differences.
    /// Returns the median of consecutive differences, or None if fewer than 2 records.
    fn compute_width_from_tgck(&self) -> Option<usize> {
        if self.tgck_records.len() < 2 {
            return None;
        }
        let mut diffs: Vec<usize> = self
            .tgck_records
            .windows(2)
            .map(|w| {
                w[1].falling_byte_index
                    .saturating_sub(w[0].falling_byte_index)
            })
            .collect();
        diffs.sort_unstable();
        Some(diffs[diffs.len() / 2])
    }

    fn write_index_entry(&self, file_num: usize) -> Result<(), std::io::Error> {
        let index_path = self.output_dir.join("captures.csv");
        let file_exists = index_path.exists();

        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&index_path)?;
        let mut writer = BufWriter::new(file);

        // Write header if file is new
        if !file_exists {
            writeln!(
                writer,
                "file_num,filename,bytes,width,start_time_us,end_time_us,duration_us,start_pos,end_pos"
            )?;
        }

        // Write metadata for this file
        let filename = format!("capture_{:04}.bin", file_num);
        let start_time_us = self.current_file_start_time_ns.unwrap_or(0) as f64 / 1_000.0;
        let end_time_us = self.current_file_end_time_ns.unwrap_or(0) as f64 / 1_000.0;
        let duration = end_time_us - start_time_us;
        let start_pos = self.current_file_start_pos.unwrap_or(0);
        let end_pos = self.current_file_end_pos.unwrap_or(0);
        let width = self.compute_width_from_tgck().unwrap_or(0);

        writeln!(
            writer,
            "{},{},{},{},{:.6},{:.6},{:.6},{},{}",
            file_num,
            filename,
            self.words_in_file,
            width,
            start_time_us,
            end_time_us,
            duration,
            start_pos,
            end_pos
        )?;

        writer.flush()?;
        Ok(())
    }

    fn open_new_file(&mut self) -> Result<(), std::io::Error> {
        // Don't close previous file here - that should be done explicitly on disable signal
        // This ensures we only create a file when we have data to write

        // Create output directory if it doesn't exist
        std::fs::create_dir_all(&self.output_dir)?;

        // Open new file
        self.file_count += 1;
        let file_path = self
            .output_dir
            .join(format!("capture_{:04}.bin", self.file_count));
        let file = File::create(&file_path)?;
        let writer = BufWriter::new(file);

        self.current_file = Some(writer);
        self.words_in_file = 0;
        self.current_file_start_time_ns = None;
        self.current_file_end_time_ns = None;
        self.current_file_start_pos = None;
        self.current_file_end_pos = None;
        self.tgck_records.clear();
        self.tgck_current_rising = None;
        self.tgck_current_falling = None;
        self.tgck_first_clock_after_rising = None;
        self.tgck_first_clock_after_falling = None;
        self.tgck_need_clock_after_rising = false;
        self.tgck_need_clock_after_falling = false;

        info!("Created new binary capture file: {}", file_path.display());
        Ok(())
    }

    fn write_word(&mut self, word: &Word) -> Result<(), std::io::Error> {
        if let Some(writer) = &mut self.current_file {
            self.words_in_file += 1;

            // Track metadata for index file
            if self.current_file_start_time_ns.is_none() {
                self.current_file_start_time_ns = Some(word.timestamp_ns);
                self.current_file_start_pos = Some(word.timestamp_ns);
            }
            self.current_file_end_time_ns = Some(word.timestamp_ns);
            self.current_file_end_pos = Some(word.timestamp_ns);

            writer.write_all(&[word.value as u8])?;
        }
        Ok(())
    }

    fn finalize_tgck_record(&mut self) {
        if let Some((rising_bi, rising_ts)) = self.tgck_current_rising.take() {
            let (falling_bi, falling_ts) = self.tgck_current_falling.take().unwrap_or((0, 0));
            let (fcr_bi, fcr_ts) = self.tgck_first_clock_after_rising.take().unwrap_or((0, 0));
            let (fcf_bi, fcf_ts) = self.tgck_first_clock_after_falling.take().unwrap_or((0, 0));
            self.tgck_records.push(TgckRecord {
                rising_byte_index: rising_bi,
                rising_timestamp: rising_ts,
                falling_byte_index: falling_bi,
                falling_timestamp: falling_ts,
                first_clock_after_rising_byte_index: fcr_bi,
                first_clock_after_rising_timestamp: fcr_ts,
                first_clock_after_falling_byte_index: fcf_bi,
                first_clock_after_falling_timestamp: fcf_ts,
            });
        }
        self.tgck_need_clock_after_rising = false;
        self.tgck_need_clock_after_falling = false;
    }

    fn write_tgck_csv(&self, file_num: usize) -> Result<(), std::io::Error> {
        if self.tgck_records.is_empty() {
            return Ok(());
        }
        let csv_path = self
            .output_dir
            .join(format!("capture_{:04}_tgck.csv", file_num));
        let file = File::create(&csv_path)?;
        let mut writer = BufWriter::new(file);
        writeln!(
            writer,
            "rising_byte_index,rising_timestamp,falling_byte_index,falling_timestamp,first_clock_rising_byte_index,first_clock_rising_timestamp,first_clock_falling_byte_index,first_clock_falling_timestamp"
        )?;
        for r in &self.tgck_records {
            writeln!(
                writer,
                "{},{},{},{},{},{},{},{}",
                r.rising_byte_index,
                r.rising_timestamp,
                r.falling_byte_index,
                r.falling_timestamp,
                r.first_clock_after_rising_byte_index,
                r.first_clock_after_rising_timestamp,
                r.first_clock_after_falling_byte_index,
                r.first_clock_after_falling_timestamp,
            )?;
        }
        writer.flush()?;
        info!(
            "Wrote TGCK CSV {} with {} records",
            csv_path.display(),
            self.tgck_records.len()
        );
        Ok(())
    }

    fn close_file(&mut self) -> Result<(), std::io::Error> {
        if let Some(mut writer) = self.current_file.take() {
            writer.flush()?;

            // Write index entry and log for non-empty files
            if self.words_in_file > 0 {
                self.finalize_tgck_record();
                self.write_index_entry(self.file_count)?;
                self.write_tgck_csv(self.file_count)?;
                info!(
                    "Closed file {} with {} words ({} bytes), {} TGCK records",
                    self.file_count,
                    self.words_in_file,
                    self.words_in_file,
                    self.tgck_records.len()
                );
            } else {
                // Delete empty files
                let file_path = self
                    .output_dir
                    .join(format!("capture_{:04}.bin", self.file_count));
                if file_path.exists() {
                    std::fs::remove_file(&file_path)?;
                    info!("Deleted empty capture file {}", self.file_count);
                }
            }
        }
        Ok(())
    }
}

impl Drop for ControlledParallelWriter {
    fn drop(&mut self) {
        // Ensure any open file is properly closed and indexed
        if self.current_file.is_some() {
            info!(">>> Writer shutting down - closing any open capture file");
            if let Err(e) = self.close_file() {
                eprintln!("Error closing file on shutdown: {}", e);
            }
        }
    }
}

impl ProcessNode for ControlledParallelWriter {
    fn name(&self) -> &str {
        "controlled_parallel_writer"
    }

    fn num_inputs(&self) -> usize {
        3 // parallel words + enable signal + tgck
    }

    fn num_outputs(&self) -> usize {
        0 // Sink
    }

    fn input_schema(&self) -> Vec<signal_processing::PortSchema> {
        use signal_processing::{PortDirection, PortSchema};
        vec![
            PortSchema::new::<Word>("parallel_words", 0, PortDirection::Input),
            PortSchema::new::<Sample>("enable_signal", 1, PortDirection::Input),
            PortSchema::new::<Sample>("tgck", 2, PortDirection::Input),
        ]
    }

    fn work(&mut self, inputs: &[InputPort], _outputs: &[OutputPort]) -> WorkResult<usize> {
        const BATCH_SIZE: usize = 1000; // Process up to 1000 words per work() call

        let mut words_buffer = std::collections::VecDeque::new();
        let mut words_input = inputs
            .first()
            .and_then(|port| port.get::<Word>(&mut words_buffer))
            .ok_or_else(|| WorkError::NodeError("Missing parallel_words input".to_string()))?;

        let mut words_processed = 0;

        while words_processed < BATCH_SIZE {
            // Get next parallel word (blocking on first iteration, try_recv after)
            let word = if words_processed == 0 {
                match words_input.recv() {
                    Ok(w) => w,
                    Err(e) => {
                        // Channel closed - close any open file before shutting down
                        info!(">>> Input channel closed - finalizing capture");
                        self.close_file().map_err(|io_err| {
                            WorkError::NodeError(format!("Failed to close file: {}", io_err))
                        })?;
                        return Err(e);
                    }
                }
            } else {
                match words_input.try_recv() {
                    Ok(w) => w,
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        info!(">>> Input channel closed - finalizing capture");
                        self.close_file().map_err(|io_err| {
                            WorkError::NodeError(format!("Failed to close file: {}", io_err))
                        })?;
                        return Err(WorkError::Shutdown);
                    }
                }
            };

            let word_position = word.timestamp_ns;

            // Collect edges up to this word's position
            // (need to drop enable_input and tgck_input before calling self.close_file)
            self.edges_to_process_buf.clear();
            self.tgck_rising_buf.clear();
            self.tgck_falling_buf.clear();
            {
                if !self.enable_channel_closed {
                    let mut enable_input = inputs
                        .get(1)
                        .and_then(|port| port.get::<Sample>(&mut self.enable_buffer))
                        .ok_or_else(|| {
                            WorkError::NodeError("Missing enable_signal input".to_string())
                        })?;

                    loop {
                        match enable_input.peek() {
                            Ok(next_edge) => {
                                if next_edge.start_time_ns <= word_position {
                                    let edge = enable_input.recv()?;
                                    if edge.value != self.current_enable_state {
                                        self.edges_to_process_buf.push(edge);
                                        // Update state to track further transitions
                                        self.current_enable_state = edge.value;
                                        self.current_enable_timestamp = edge.start_time_ns;
                                    }
                                } else {
                                    break;
                                }
                            }
                            Err(WorkError::Shutdown) => {
                                debug!("[{}] Enable input channel closed", self.name());
                                self.enable_channel_closed = true;
                                break;
                            }
                            Err(e) => return Err(e),
                        }
                    }
                }

                // Consume TGCK edges up to this word's position
                if !self.tgck_channel_closed {
                    let mut tgck_input = inputs
                        .get(2)
                        .and_then(|port| port.get::<Sample>(&mut self.tgck_buffer))
                        .ok_or_else(|| WorkError::NodeError("Missing tgck input".to_string()))?;

                    loop {
                        match tgck_input.peek() {
                            Ok(next_edge) => {
                                if next_edge.start_time_ns <= word_position {
                                    let edge = tgck_input.recv()?;
                                    // Detect rising edge
                                    if edge.value && !self.last_tgck_value {
                                        self.tgck_rising_buf.push(edge.start_time_ns);
                                    }
                                    // Detect falling edge
                                    if !edge.value && self.last_tgck_value {
                                        self.tgck_falling_buf.push(edge.start_time_ns);
                                    }
                                    self.last_tgck_value = edge.value;
                                } else {
                                    break;
                                }
                            }
                            Err(WorkError::Shutdown) => {
                                debug!("[{}] TGCK input channel closed", self.name());
                                self.tgck_channel_closed = true;
                                break;
                            }
                            Err(e) => return Err(e),
                        }
                    }
                }
            } // enable_input and tgck_input dropped here

            // Record TGCK rising edges — each starts a new record
            for i in 0..self.tgck_rising_buf.len() {
                let ts = self.tgck_rising_buf[i];
                if self.current_file.is_some() {
                    self.finalize_tgck_record();
                    self.tgck_current_rising = Some((self.words_in_file, ts));
                    self.tgck_need_clock_after_rising = true;
                }
            }

            // Record TGCK falling edges
            for i in 0..self.tgck_falling_buf.len() {
                let ts = self.tgck_falling_buf[i];
                if self.current_file.is_some() {
                    self.tgck_current_falling = Some((self.words_in_file, ts));
                    self.tgck_need_clock_after_falling = true;
                }
            }

            // Process each state transition
            let mut i = 0;
            while i < self.edges_to_process_buf.len() {
                let edge = self.edges_to_process_buf[i];
                if !edge.value {
                    info!(
                        ">>> Enable INACTIVE at position {} - closing capture file",
                        edge.start_time_ns,
                    );
                    self.close_file().map_err(|e| {
                        WorkError::NodeError(format!("Failed to close file: {}", e))
                    })?;
                } else {
                    info!(">>> Enable ACTIVE at position {}", edge.start_time_ns,);
                }
                i += 1;
            }

            // Write word if currently enabled
            if self.current_enable_state {
                // Open file if needed (first word after enable)
                if self.current_file.is_none() {
                    info!(
                        ">>> First word while enabled - new capture file (enabled at position {}, word at {} ns)",
                        self.current_enable_timestamp, word.timestamp_ns
                    );
                    self.open_new_file()
                        .map_err(|e| WorkError::NodeError(format!("Failed to open file: {}", e)))?;
                }

                // Track first clock (strobe) edge after TGCK events
                if self.tgck_need_clock_after_rising {
                    self.tgck_first_clock_after_rising =
                        Some((self.words_in_file, word.timestamp_ns));
                    self.tgck_need_clock_after_rising = false;
                }
                if self.tgck_need_clock_after_falling {
                    self.tgck_first_clock_after_falling =
                        Some((self.words_in_file, word.timestamp_ns));
                    self.tgck_need_clock_after_falling = false;
                }

                self.count += 1;
                self.write_word(&word)
                    .map_err(|e| WorkError::NodeError(format!("Failed to write word: {}", e)))?;

                // Log progress every 100000 words (reduced from 10000 for performance)
                if self.count.is_multiple_of(100000) {
                    info!(
                        "Progress: {} words written (latest: 0x{:02X} at t={} ns in file {})",
                        self.count, word.value, word.timestamp_ns, self.file_count
                    );
                }
            }

            words_processed += 1;
        }

        Ok(words_processed)
    }
}

/// Sink that monitors enable/disable state
struct StateMonitor;

impl ProcessNode for StateMonitor {
    fn name(&self) -> &str {
        "state_monitor"
    }

    fn num_inputs(&self) -> usize {
        1
    }

    fn num_outputs(&self) -> usize {
        0 // Sink
    }

    fn input_schema(&self) -> Vec<signal_processing::PortSchema> {
        use signal_processing::{PortDirection, PortSchema, Sample};
        vec![PortSchema::new::<Sample>(
            "enable_state",
            0,
            PortDirection::Input,
        )]
    }

    fn work(&mut self, inputs: &[InputPort], _outputs: &[OutputPort]) -> WorkResult<usize> {
        let mut input_buffer = std::collections::VecDeque::new();
        let mut input = inputs
            .first()
            .and_then(|port| port.get::<Sample>(&mut input_buffer))
            .ok_or_else(|| WorkError::NodeError("Missing input channel".to_string()))?;

        // Use try_recv() to avoid blocking and contributing to backpressure
        match input.try_recv() {
            Ok(state) => {
                info!(
                    ">>> Parallel decoder {}",
                    if state.value { "ENABLED" } else { "DISABLED" }
                );
                Ok(1)
            }
            Err(TryRecvError::Empty) => Ok(0), // No data available, return and let scheduler call again
            Err(TryRecvError::Disconnected) => {
                info!("State monitor: input channel disconnected, shutting down");
                Err(WorkError::Shutdown)
            }
        }
    }
}

/// Sink that monitors SPI transfers
struct SpiMonitor;

impl ProcessNode for SpiMonitor {
    fn name(&self) -> &str {
        "spi_monitor"
    }

    fn num_inputs(&self) -> usize {
        1
    }

    fn num_outputs(&self) -> usize {
        0 // Sink
    }

    fn input_schema(&self) -> Vec<signal_processing::PortSchema> {
        use signal_processing::{PortDirection, PortSchema};
        vec![PortSchema::new::<Word>(
            "spi_transfers",
            0,
            PortDirection::Input,
        )]
    }

    fn work(&mut self, inputs: &[InputPort], _outputs: &[OutputPort]) -> WorkResult<usize> {
        let mut input_buffer = std::collections::VecDeque::new();
        let mut input = inputs
            .first()
            .and_then(|port| port.get::<Word>(&mut input_buffer))
            .ok_or_else(|| WorkError::NodeError("Missing input channel".to_string()))?;

        // Use try_recv() to avoid blocking and contributing to backpressure
        match input.try_recv() {
            Ok(transfer) => {
                // Print the SPI command (only show enable/disable commands to reduce log spam)
                if transfer.value == 0x600081 || transfer.value == 0x600000 {
                    info!(
                        "SPI Command: 0x{:06X} at t={} ns",
                        transfer.value, transfer.timestamp_ns
                    );
                }
                Ok(1)
            }
            Err(TryRecvError::Empty) => Ok(0), // No data available, return and let scheduler call again
            Err(TryRecvError::Disconnected) => {
                info!("SPI monitor: input channel disconnected, shutting down");
                Err(WorkError::Shutdown)
            }
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize tracing subscriber
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let args = Args::parse();

    info!("=== SPI-Controlled Parallel Decode Example ===");
    info!("File: {}", args.file);

    info!(
        "SPI: CS={}, CLK={}, MOSI={}",
        args.spi_cs, args.spi_clk, args.spi_mosi
    );
    info!(
        "Parallel: Strobe={}, Data={:?}, TGCK={}",
        args.parallel_strobe, args.parallel_data, args.tgck
    );
    info!("Enable command: 0x{:06X}", args.enable_cmd);
    info!("Disable command: 0x{:06X}", args.disable_cmd);
    info!("Output directory: {}", args.output_dir.display());

    // Calculate total channels needed
    let max_channel = *[
        args.spi_cs,
        args.spi_clk,
        args.spi_mosi,
        args.parallel_strobe,
        args.tgck,
    ]
    .iter()
    .chain(args.parallel_data.iter())
    .max()
    .unwrap();

    let num_channels = (max_channel + 1) as u8;
    info!("Using {} channels", num_channels);

    // Create pipeline with large buffers for raw signal channels (high bandwidth)
    // Control signals and decoded data will use smaller buffers via connect_with_buffer
    // Watchdog is always enabled - reports operations blocked >5 seconds
    let mut pipeline = Pipeline::new().with_default_buffer_size(10_000_000);

    // Add file source (autodetects 0 inputs, N outputs)
    let source = DslFileSource::new(&args.file, num_channels)?;

    pipeline.add_process("source", source)?;

    // Add SPI decoder (autodetects 3 inputs, 1 output)
    let spi_decoder = SpiDecoder::new(
        SpiMode::Mode0,
        24,    // 24-bit words for commands like 0x600081
        true,  // has_mosi
        false, // has_miso - decoder uses 3 inputs: CS, CLK, MOSI
    );

    pipeline.add_process("spi_decoder", spi_decoder)?;

    // Wire SPI decoder inputs from source
    pipeline.connect(
        "source",
        &format!("ch{}", args.spi_clk),
        "spi_decoder",
        "clk",
    )?;
    pipeline.connect("source", &format!("ch{}", args.spi_cs), "spi_decoder", "cs")?;
    pipeline.connect(
        "source",
        &format!("ch{}", args.spi_mosi),
        "spi_decoder",
        "mosi",
    )?;

    // Add SPI command controller (autodetects 1 input, 1 output)
    pipeline.add_process(
        "controller",
        SpiCommandController::new(args.enable_cmd, args.disable_cmd),
    )?;

    // Add SPI monitor sink (autodetects 1 input, 0 outputs)
    pipeline.add_process("spi_monitor", SpiMonitor)?;

    // Connect SPI decoder output to both monitor and controller (small buffers for low-bandwidth SPI commands)
    pipeline.connect_with_buffer(
        "spi_decoder",
        "mosi_words",
        "spi_monitor",
        "spi_transfers",
        1000,
    )?;
    pipeline.connect_with_buffer("spi_decoder", "mosi_words", "controller", "spi_in", 1000)?;

    // Add state monitor (autodetects 1 input, 0 outputs)
    pipeline.add_process("state_monitor", StateMonitor)?;

    // Connect enable signals with small buffers (control signals don't need large buffers)
    pipeline.connect_with_buffer(
        "controller",
        "enable_signal",
        "state_monitor",
        "enable_state",
        100,
    )?;

    // Add parallel decoder - requires enable_signal and CS inputs (autodetects inputs/outputs)
    pipeline.add_process(
        "parallel_decoder",
        ParallelDecoder::new(
            args.parallel_data.len(),
            StrobeMode::AnyEdge,
            CsPolarity::ActiveLow,
        ),
    )?;

    // Wire parallel decoder strobe from source (block channel, small buffer — each block is ~2MB)
    pipeline.connect_with_buffer(
        "source",
        &format!("ch{}", args.parallel_strobe),
        "parallel_decoder",
        "strobe",
        4,
    )?;

    // Wire parallel decoder data bits from source (block channels)
    for (i, &channel) in args.parallel_data.iter().enumerate() {
        pipeline.connect_with_buffer(
            "source",
            &format!("ch{}", channel),
            "parallel_decoder",
            &format!("d{}", i),
            4,
        )?;
    }

    // Wire CS from source to parallel_decoder (block channel)
    pipeline.connect_with_buffer(
        "source",
        &format!("ch{}", args.spi_cs),
        "parallel_decoder",
        "cs",
        4,
    )?;

    // Wire enable_signal from controller to parallel_decoder (edge-based Sample, small buffer)
    pipeline.connect_with_buffer(
        "controller",
        "enable_signal",
        "parallel_decoder",
        "enable_signal",
        100,
    )?;

    // Add parallel word writer (autodetects 2 inputs, 0 outputs)
    pipeline.add_process(
        "writer",
        ControlledParallelWriter::new(args.output_dir.clone()),
    )?;

    // Connect parallel words with large buffer for high throughput
    pipeline.connect_with_buffer(
        "parallel_decoder",
        "words",
        "writer",
        "parallel_words",
        100_000, // Large buffer for performance on big files
    )?;

    // Connect enable signal to writer (small buffer for control signal)
    pipeline.connect_with_buffer(
        "controller",
        "enable_signal",
        "writer",
        "enable_signal",
        100,
    )?;

    // Connect TGCK from source to writer
    pipeline.connect("source", &format!("ch{}", args.tgck), "writer", "tgck")?;

    // Build and run
    info!("Building pipeline...");
    let scheduler = pipeline.build()?;

    info!("Running...");
    scheduler.wait();

    info!("Done!");

    Ok(())
}
