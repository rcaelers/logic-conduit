//! Text-line file writer rolled over by a filename level.
//!
//! Same shape as [`BinaryFileWriter`](super::binary_file_writer::BinaryFileWriter),
//! generalized to arbitrary text lines instead of decoded words: any
//! producer of `TextSample` events (CSV rows, log lines, …) paired with a
//! `TextSample` filename level can be captured to disk without knowing
//! anything about files itself. [`TgckRecorder`](super::tgck_recorder::TgckRecorder)
//! is the first user — it emits CSV rows and a derived filename instead of
//! writing directly, so its correlation logic stays platform-agnostic.
//!
//! The writer blocks on its `lines` input only — never on `filename`. Per
//! the level-stream contract the filename is always defined (its producer
//! emits the initial value at t=0), so the writer keeps a *current filename*
//! register and applies filename changes as their timestamps are passed by
//! the line stream. Files open lazily on the first line written to them, so
//! name windows without lines produce no file at all.

use std::collections::VecDeque;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::PathBuf;

use tracing::{debug, info, warn};

use signal_processing::errors::{WorkError, WorkResult};
use signal_processing::events::TextSample;
use signal_processing::node::ProcessNode;
use signal_processing::ports::{InputPort, OutputPort, PortDirection, PortSchema};

/// Sink appending [`TextSample`] lines to files named by another
/// [`TextSample`] level.
///
/// Inputs: `lines` (0) — `TextSample` events, one written per line;
/// `filename` (1) — `TextSample` level.
/// Outputs: none.
pub struct TextFileWriter {
    name: String,

    lines_buffer: VecDeque<TextSample>,
    name_buffer: VecDeque<TextSample>,
    /// Drained but not yet applicable name changes (timestamps ahead of the
    /// line stream), in channel (= timestamp) order.
    pending_names: VecDeque<TextSample>,

    current_name: Option<String>,
    current_file: Option<BufWriter<File>>,
    lines_in_file: u64,
    last_line_ts: u64,
}

impl TextFileWriter {
    pub fn new() -> Self {
        Self {
            name: "text_file_writer".to_string(),
            lines_buffer: VecDeque::new(),
            name_buffer: VecDeque::new(),
            pending_names: VecDeque::new(),
            current_name: None,
            current_file: None,
            lines_in_file: 0,
            last_line_ts: 0,
        }
    }

    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    /// Flush and close the current file, if any.
    fn close_current(&mut self) -> std::io::Result<()> {
        if let Some(mut writer) = self.current_file.take() {
            writer.flush()?;
            info!(
                "[{}] closed {} ({} lines)",
                self.name,
                self.current_name.as_deref().unwrap_or_default(),
                self.lines_in_file
            );
        } else if self.lines_in_file == 0 && self.current_name.is_some() {
            debug!(
                "[{}] name window {:?} had no lines, no file created",
                self.name, self.current_name
            );
        }
        self.lines_in_file = 0;
        Ok(())
    }

    /// Switch to a new filename, closing the current file.
    fn apply_name_change(&mut self, change: TextSample) -> WorkResult<()> {
        if change.start_time_ns < self.last_line_ts {
            warn!(
                "[{}] filename change to {:?} at {}ns arrived after data at {}ns — \
                 lines may have landed at the previous boundary",
                self.name, change.value, change.start_time_ns, self.last_line_ts
            );
        }
        self.close_current()
            .map_err(|e| WorkError::NodeError(format!("closing file: {e}")))?;
        debug!(
            "[{}] filename -> {:?} at {}ns",
            self.name, change.value, change.start_time_ns
        );
        self.current_name = Some(change.value);
        Ok(())
    }

    fn ensure_file_open(&mut self) -> WorkResult<&mut BufWriter<File>> {
        if self.current_file.is_none() {
            let name = self
                .current_name
                .as_deref()
                .ok_or_else(|| WorkError::NodeError("No filename set".to_string()))?;
            let path = PathBuf::from(name);
            if let Some(parent) = path.parent()
                && !parent.as_os_str().is_empty()
            {
                std::fs::create_dir_all(parent)
                    .map_err(|e| WorkError::NodeError(format!("creating {parent:?}: {e}")))?;
            }
            let file = File::create(&path)
                .map_err(|e| WorkError::NodeError(format!("creating {path:?}: {e}")))?;
            info!("[{}] created {}", self.name, path.display());
            self.current_file = Some(BufWriter::new(file));
        }
        Ok(self.current_file.as_mut().unwrap())
    }
}

impl Default for TextFileWriter {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for TextFileWriter {
    fn drop(&mut self) {
        if self.current_file.is_some() {
            info!("[{}] shutting down — closing open file", self.name);
            if let Err(e) = self.close_current() {
                warn!("[{}] error closing file on shutdown: {e}", self.name);
            }
        }
    }
}

impl ProcessNode for TextFileWriter {
    fn name(&self) -> &str {
        &self.name
    }

    fn num_inputs(&self) -> usize {
        2
    }

    fn num_outputs(&self) -> usize {
        0
    }

    fn input_schema(&self) -> Vec<PortSchema> {
        vec![
            PortSchema::new::<TextSample>("lines", 0, PortDirection::Input),
            PortSchema::new::<TextSample>("filename", 1, PortDirection::Input),
        ]
    }

    fn output_schema(&self) -> Vec<PortSchema> {
        vec![]
    }

    fn work(&mut self, inputs: &[InputPort], _outputs: &[OutputPort]) -> WorkResult<usize> {
        let mut lines = inputs
            .first()
            .and_then(|port| port.get::<TextSample>(&mut self.lines_buffer))
            .ok_or_else(|| WorkError::NodeError("Missing lines input".to_string()))?;
        let mut names = inputs
            .get(1)
            .and_then(|port| port.get::<TextSample>(&mut self.name_buffer))
            .ok_or_else(|| WorkError::NodeError("Missing filename input".to_string()))?;

        // The initial name is guaranteed by the level-stream contract (sent
        // at t=0), so a blocking wait for it is bounded and only happens once.
        if self.current_name.is_none() && self.pending_names.is_empty() {
            let initial = names.recv()?;
            self.pending_names.push_back(initial);
        }

        // Block for a line; on shutdown finalize the open file.
        let line = match lines.recv() {
            Ok(line) => line,
            Err(WorkError::Shutdown) => {
                self.close_current()
                    .map_err(|e| WorkError::NodeError(format!("closing file: {e}")))?;
                return Err(WorkError::Shutdown);
            }
            Err(e) => return Err(e),
        };

        // Opportunistically drain name changes (never blocks).
        while let Ok(change) = names.try_recv() {
            self.pending_names.push_back(change);
        }

        // Apply every name change the line stream has passed.
        let line_ts = line.start_time_ns;
        while self
            .pending_names
            .front()
            .is_some_and(|change| change.start_time_ns <= line_ts)
        {
            let change = self.pending_names.pop_front().unwrap();
            self.apply_name_change(change)?;
        }
        self.last_line_ts = line_ts;

        let writer = self.ensure_file_open()?;
        writer
            .write_all(line.value.as_bytes())
            .and_then(|_| writer.write_all(b"\n"))
            .map_err(|e| WorkError::NodeError(format!("writing line: {e}")))?;
        self.lines_in_file += 1;
        Ok(1)
    }
}

#[cfg(test)]
mod tests {
    use crossbeam_channel::bounded;
    use signal_processing::sender::ChannelMessage;
    use signal_processing::watchdog::Watchdog;

    use super::*;

    struct Rig {
        lines_tx: crossbeam_channel::Sender<ChannelMessage<TextSample>>,
        name_tx: crossbeam_channel::Sender<ChannelMessage<TextSample>>,
        inputs: Vec<InputPort>,
    }

    fn rig() -> Rig {
        let wd = Watchdog::new();
        let (lines_tx, lines_rx) = bounded::<ChannelMessage<TextSample>>(256);
        let (name_tx, name_rx) = bounded::<ChannelMessage<TextSample>>(256);
        Rig {
            lines_tx,
            name_tx,
            inputs: vec![
                InputPort::new_with_watchdog(lines_rx, &wd, "writer", "lines"),
                InputPort::new_with_watchdog(name_rx, &wd, "writer", "filename"),
            ],
        }
    }

    fn run(rig: Rig, writer: &mut TextFileWriter) {
        let Rig {
            lines_tx,
            name_tx,
            inputs,
        } = rig;
        drop(lines_tx);
        drop(name_tx);
        loop {
            match writer.work(&inputs, &[]) {
                Ok(_) => {}
                Err(WorkError::Shutdown) => break,
                Err(e) => panic!("unexpected error: {e}"),
            }
        }
    }

    #[test]
    fn writes_lines_to_named_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.csv").display().to_string();

        let rig = rig();
        rig.name_tx
            .send(ChannelMessage::Sample(TextSample::new(&path, 0)))
            .unwrap();
        for (line, ts) in [("a,b,c", 10u64), ("1,2,3", 20)] {
            rig.lines_tx
                .send(ChannelMessage::Sample(TextSample::new(line, ts)))
                .unwrap();
        }

        run(rig, &mut TextFileWriter::new());

        assert_eq!(std::fs::read_to_string(&path).unwrap(), "a,b,c\n1,2,3\n");
    }

    #[test]
    fn rolls_files_on_name_changes() {
        let dir = tempfile::tempdir().unwrap();
        let path = |n: &str| dir.path().join(n).display().to_string();

        let rig = rig();
        rig.name_tx
            .send(ChannelMessage::Sample(TextSample::new(path("a.csv"), 0)))
            .unwrap();
        rig.name_tx
            .send(ChannelMessage::Sample(TextSample::new(
                path("b.csv"),
                1_000,
            )))
            .unwrap();
        for (line, ts) in [("one", 100u64), ("two", 200), ("three", 1_000)] {
            rig.lines_tx
                .send(ChannelMessage::Sample(TextSample::new(line, ts)))
                .unwrap();
        }

        run(rig, &mut TextFileWriter::new());

        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.csv")).unwrap(),
            "one\ntwo\n"
        );
        // The line at exactly the boundary timestamp lands in the new file.
        assert_eq!(
            std::fs::read_to_string(dir.path().join("b.csv")).unwrap(),
            "three\n"
        );
    }

    #[test]
    fn empty_name_window_creates_no_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = |n: &str| dir.path().join(n).display().to_string();

        let rig = rig();
        rig.name_tx
            .send(ChannelMessage::Sample(TextSample::new(path("a.csv"), 0)))
            .unwrap();
        rig.name_tx
            .send(ChannelMessage::Sample(TextSample::new(path("b.csv"), 500)))
            .unwrap();
        // All lines arrive after the second name — window "a" stays empty.
        rig.lines_tx
            .send(ChannelMessage::Sample(TextSample::new("only", 600)))
            .unwrap();

        run(rig, &mut TextFileWriter::new());

        assert!(!dir.path().join("a.csv").exists());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("b.csv")).unwrap(),
            "only\n"
        );
    }

    #[test]
    fn shutdown_flushes_open_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("only.csv").display().to_string();

        let rig = rig();
        rig.name_tx
            .send(ChannelMessage::Sample(TextSample::new(&path, 0)))
            .unwrap();
        for i in 0..10u64 {
            rig.lines_tx
                .send(ChannelMessage::Sample(TextSample::new(
                    format!("line{i}"),
                    100 + i,
                )))
                .unwrap();
        }

        run(rig, &mut TextFileWriter::new());

        let content = std::fs::read_to_string(&path).unwrap();
        assert_eq!(content.lines().count(), 10);
        assert_eq!(content.lines().last(), Some("line9"));
    }
}
