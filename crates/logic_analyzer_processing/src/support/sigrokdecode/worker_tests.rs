use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use pyo3::Python;
use pyo3::types::{PyAnyMethods, PyStringMethods};

use super::python_host::{OUTPUT_ANN, OUTPUT_PYTHON};
use super::scheduler::{InitialPin, LogicChunk};
use super::worker::{DecoderWorker, OptionValue, WorkerConfig, WorkerError, WorkerInputConfig};

const TIMEOUT: Duration = Duration::from_secs(5);

#[test]
fn worker_runs_wait_register_put_and_channel_presence_to_eof() {
    let fixture = DecoderFixture::new(
        "bridge_decoder",
        r#"
import sigrokdecode as srd

class Decoder(srd.Decoder):
    def __init__(self):
        pass
    def metadata(self, key, value):
        self.samplerate = value
    def start(self):
        assert self.has_channel(0)
        assert not self.has_channel(1)
        self.ann = self.register(srd.OUTPUT_ANN)
        self.python = self.register(srd.OUTPUT_PYTHON, proto_id='fixture')
    def decode(self):
        pins = self.wait({0: 'r'})
        self.put(self.samplenum, self.samplenum + 1, self.ann, [0, [str(pins[0])]])
        self.wait([{'skip': 1}, {0: 'f'}])
        self.put(self.samplenum, self.samplenum + 1, self.python, ['fixture', 7])
        while True:
            self.wait()
"#,
    );
    let mut worker = fixture.spawn(vec![Some(InitialPin::SameAsFirstSample), None]);
    worker
        .push_chunk(chunk(0, 5, &[false, false, true, true, false]))
        .unwrap();

    let first = worker.receive_output(TIMEOUT).unwrap().unwrap();
    let second = worker.receive_output(TIMEOUT).unwrap().unwrap();
    assert_eq!(
        (first.start_sample, first.end_sample, first.output_id),
        (2, 3, 0)
    );
    assert_eq!(
        (second.start_sample, second.end_sample, second.output_id),
        (3, 4, 1)
    );
    Python::attach(|py| {
        assert_eq!(
            first.data.bind(py).str().unwrap().to_str().unwrap(),
            "[0, ['1']]"
        );
        assert_eq!(
            second.data.bind(py).str().unwrap().to_str().unwrap(),
            "['fixture', 7]"
        );
    });
    assert_eq!(
        worker
            .registrations()
            .iter()
            .map(|registration| (
                registration.output_type,
                registration.protocol_id.as_deref()
            ))
            .collect::<Vec<_>>(),
        [(OUTPUT_ANN, None), (OUTPUT_PYTHON, Some("fixture"))]
    );

    worker.finish().unwrap();
    worker.join().unwrap();
}

#[test]
fn worker_reports_python_traceback() {
    let fixture = DecoderFixture::new(
        "failing_decoder",
        r#"
import sigrokdecode as srd

class Decoder(srd.Decoder):
    def metadata(self, key, value):
        pass
    def start(self):
        pass
    def decode(self):
        raise RuntimeError('fixture exploded')
"#,
    );
    let mut worker = fixture.spawn(vec![Some(InitialPin::Low)]);
    let error = worker.join().unwrap_err();
    let WorkerError::Python(traceback) = error else {
        panic!("expected Python error");
    };
    assert!(traceback.contains("Traceback (most recent call last)"));
    assert!(traceback.contains("fixture exploded"));
    assert!(traceback.contains("in decode"));
}

#[test]
fn cancelling_a_blocked_worker_wakes_and_joins_cleanly() {
    let fixture = DecoderFixture::new(
        "waiting_decoder",
        r#"
import sigrokdecode as srd

class Decoder(srd.Decoder):
    def metadata(self, key, value):
        pass
    def start(self):
        pass
    def decode(self):
        while True:
            self.wait({0: 'e'})
"#,
    );
    let mut worker = fixture.spawn(vec![Some(InitialPin::Low)]);
    worker.cancel();
    worker.join().unwrap();
}

#[test]
fn unmodified_spi_decoder_runs_through_the_worker() {
    let Some(decoder_root) = local_decoder_root() else {
        eprintln!("skipping Sigrok SPI worker test: set SIGROK_DECODERS_DIR");
        return;
    };
    let options = BTreeMap::from([
        ("bitorder".into(), OptionValue::String("msb-first".into())),
        (
            "cs_polarity".into(),
            OptionValue::String("active-low".into()),
        ),
        ("cpol".into(), OptionValue::Integer(0)),
        ("cpha".into(), OptionValue::Integer(0)),
        ("wordsize".into(), OptionValue::Integer(8)),
    ]);
    let config = WorkerConfig {
        decoder_root,
        decoder_id: "spi".into(),
        sample_rate: 1_000_000,
        input: WorkerInputConfig::Logic(vec![
            Some(InitialPin::SameAsFirstSample),
            None,
            Some(InitialPin::SameAsFirstSample),
            None,
        ]),
        options,
        queue_capacity: 128,
    };
    let mut worker = DecoderWorker::spawn(config).unwrap();
    let (clock, mosi) = spi_samples(0xa5);
    worker
        .push_chunk(LogicChunk::new(
            0,
            clock.len(),
            vec![Some(pack(&clock)), None, Some(pack(&mosi)), None],
        ))
        .unwrap();
    worker.finish().unwrap();
    worker.join().unwrap();

    let mut saw_python_word = false;
    while let Some(output) = worker.try_output().unwrap() {
        let registration = &worker.registrations()[output.output_id];
        if registration.output_type == OUTPUT_PYTHON {
            saw_python_word = true;
        }
    }
    assert!(saw_python_word, "SPI decoder emitted no protocol word");
}

fn chunk(start: u64, count: usize, samples: &[bool]) -> LogicChunk {
    LogicChunk::new(start, count, vec![Some(pack(samples)), None])
}

fn pack(samples: &[bool]) -> Arc<[u8]> {
    let mut packed = vec![0_u8; samples.len().div_ceil(8)];
    for (sample, high) in samples.iter().copied().enumerate() {
        if high {
            packed[sample / 8] |= 1 << (sample % 8);
        }
    }
    packed.into()
}

fn spi_samples(word: u8) -> (Vec<bool>, Vec<bool>) {
    let mut clock = vec![false];
    let mut mosi = vec![word & 0x80 != 0];
    for bit in (0..8).rev() {
        let value = word & (1 << bit) != 0;
        clock.extend([true, false]);
        mosi.extend([value, value]);
    }
    (clock, mosi)
}

fn local_decoder_root() -> Option<PathBuf> {
    std::env::var_os("SIGROK_DECODERS_DIR")
        .map(PathBuf::from)
        .or_else(|| {
            Some(
                PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("../../../dslogic/libsigrokdecode/decoders"),
            )
        })
        .filter(|path| path.join("spi/pd.py").is_file())
}

struct DecoderFixture {
    _directory: tempfile::TempDir,
    root: PathBuf,
    id: String,
}

impl DecoderFixture {
    fn new(id: &str, source: &str) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let package = directory.path().join(id);
        fs::create_dir(&package).unwrap();
        fs::write(package.join("__init__.py"), "from .pd import Decoder\n").unwrap();
        fs::write(package.join("pd.py"), source).unwrap();
        Self {
            root: directory.path().to_owned(),
            _directory: directory,
            id: id.to_owned(),
        }
    }

    fn spawn(&self, channels: Vec<Option<InitialPin>>) -> DecoderWorker {
        DecoderWorker::spawn(WorkerConfig {
            decoder_root: self.root.clone(),
            decoder_id: self.id.clone(),
            sample_rate: 1_000_000,
            input: WorkerInputConfig::Logic(channels),
            options: BTreeMap::new(),
            queue_capacity: 16,
        })
        .unwrap()
    }
}
