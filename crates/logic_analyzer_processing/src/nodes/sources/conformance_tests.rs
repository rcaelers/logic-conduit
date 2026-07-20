use std::time::{Duration, Instant};

use tempfile::tempdir;

use signal_processing::{
    AcquisitionContext, AcquisitionResult, CaptureAcquisitionPhase, CaptureChannelId,
    CaptureCursorItem, CaptureDataDelivery, CaptureEvent, CaptureProviderCapabilities,
    CaptureQueueReceiveError, CaptureSessionId, CaptureSessionState, CaptureStoreCursor,
    CaptureStoreDescriptor, NativeCaptureStore, NativeCaptureStoreConfig, NativeFinalizedCapture,
    PreparedAcquisition, SimpleTriggerCondition, bounded_capture_event_queue,
};

use super::buffered_fake::{BufferedFakeConfig, BufferedFakeProvider};
use super::demo_capture_source::{DeterministicFakeConfig, DeterministicFakeProvider};

const TIMEOUT: Duration = Duration::from_secs(2);

struct ProviderContractCase {
    name: &'static str,
    channels: Vec<CaptureChannelId>,
    total_samples: u64,
    expected_trigger: u64,
    expected_delivery: CaptureDataDelivery,
    capabilities: CaptureProviderCapabilities,
    prepare: Box<
        dyn FnOnce(AcquisitionContext) -> AcquisitionResult<Box<dyn PreparedAcquisition>> + Send,
    >,
    level_at: Box<dyn Fn(u64, usize) -> bool + Send + Sync>,
}

fn run_provider_contract(case: ProviderContractCase) {
    let session_id = CaptureSessionId::new(0x7000 + case.expected_trigger as u128);
    assert!(
        case.capabilities.supports(
            &case.channels,
            case.capabilities.setting_matrix()[0].sample_rates_hz()[0] as f64
        ),
        "{} active setting must be advertised",
        case.name
    );
    assert_eq!(case.capabilities.data_delivery(), case.expected_delivery);
    assert!(!case.capabilities.supports_force_trigger());

    let temporary = tempdir().unwrap();
    let descriptor = CaptureStoreDescriptor::new(session_id, case.channels.clone()).unwrap();
    let (store, writer) = NativeCaptureStore::create(
        NativeCaptureStoreConfig::new(temporary.path(), descriptor)
            .with_commit_batch_chunks(1)
            .unwrap(),
    )
    .unwrap();
    let (events, event_reader) = bounded_capture_event_queue(128).unwrap();
    let context = AcquisitionContext::new(session_id, Box::new(writer), Box::new(events));
    let mut acquisition = (case.prepare)(context).unwrap();
    acquisition.start().unwrap();
    let outcome = acquisition.join().unwrap();
    assert_eq!(
        outcome.captured_samples, case.total_samples,
        "{}",
        case.name
    );

    let finalized = store.finalize().unwrap();
    let reopened = NativeFinalizedCapture::open(finalized.directory()).unwrap();
    let mut cursor = reopened.open_cursor().unwrap();
    let mut reconstructed = 0_u64;
    let mut sequence = 0_u64;
    loop {
        match cursor.next().unwrap() {
            CaptureCursorItem::Chunk(chunk) => {
                assert_eq!(chunk.sequence(), sequence, "{}", case.name);
                assert_eq!(chunk.start_sample(), reconstructed, "{}", case.name);
                assert_eq!(chunk.channels(), case.channels, "{}", case.name);
                for relative in 0..chunk.sample_count() {
                    for channel in 0..case.channels.len() {
                        assert_eq!(
                            chunk.packed_level(relative, channel),
                            Some((case.level_at)(reconstructed + relative, channel)),
                            "{} sample {} channel {}",
                            case.name,
                            reconstructed + relative,
                            channel
                        );
                    }
                }
                reconstructed = chunk.end_sample();
                sequence += 1;
            }
            CaptureCursorItem::End => break,
            CaptureCursorItem::Pending => panic!("{} finalized cursor is pending", case.name),
        }
    }
    assert_eq!(reconstructed, case.total_samples, "{}", case.name);

    let mut states = Vec::new();
    let mut phases = Vec::new();
    let mut trigger = None;
    loop {
        match event_reader.recv_timeout(TIMEOUT) {
            Ok(CaptureEvent::Status(status)) => {
                states.push(status.state);
                phases.push(status.phase);
            }
            Ok(CaptureEvent::Triggered { sample, .. }) => trigger = Some(sample),
            Ok(CaptureEvent::Progress { .. } | CaptureEvent::Health { .. }) => {}
            Ok(CaptureEvent::Plan { .. }) => {}
            Ok(CaptureEvent::Failed(failure)) => {
                panic!("{} failed unexpectedly: {failure:?}", case.name)
            }
            Err(CaptureQueueReceiveError::Closed) => break,
            Err(error) => panic!("{} event error: {error}", case.name),
        }
    }
    assert_eq!(trigger, Some(case.expected_trigger), "{}", case.name);
    for required in [
        CaptureSessionState::Preparing,
        CaptureSessionState::Prepared,
        CaptureSessionState::Armed,
        CaptureSessionState::Triggered,
        CaptureSessionState::Recording,
        CaptureSessionState::Stopping,
        CaptureSessionState::Complete,
    ] {
        assert!(
            states.contains(&required),
            "{} lacks {required:?}",
            case.name
        );
    }
    match case.expected_delivery {
        CaptureDataDelivery::DuringAcquisition => assert!(
            phases.contains(&CaptureAcquisitionPhase::ReceivingLiveData),
            "{}",
            case.name
        ),
        CaptureDataDelivery::BufferedUpload => assert!(
            phases.contains(&CaptureAcquisitionPhase::CapturingOnDevice)
                && phases.contains(&CaptureAcquisitionPhase::UploadingBufferedData),
            "{}",
            case.name
        ),
    }
}

#[test]
fn streaming_and_buffered_providers_pass_the_same_raw_trigger_contract() {
    let streaming_channels = (0..19)
        .map(|channel| {
            CaptureChannelId::new(format!("stream-bank-{}:{}", channel % 4, channel * 7 + 3))
        })
        .collect::<Vec<_>>();
    let mut streaming_trigger_conditions = vec![None; streaming_channels.len()];
    streaming_trigger_conditions[0] = Some(SimpleTriggerCondition::Rising);
    let streaming =
        DeterministicFakeConfig::new(streaming_channels.clone(), vec![3, 5, 7, 4], 0x5a17)
            .unwrap()
            .with_simple_trigger(streaming_trigger_conditions)
            .unwrap();
    let streaming_trigger = streaming.first_trigger_sample().unwrap();
    let streaming_levels = streaming.clone();
    let streaming_capabilities = CaptureProviderCapabilities::single(
        CaptureDataDelivery::DuringAcquisition,
        streaming_channels.clone(),
        1_000_000,
    );
    run_provider_contract(ProviderContractCase {
        name: "streaming fake",
        channels: streaming_channels,
        total_samples: streaming.total_samples(),
        expected_trigger: streaming_trigger,
        expected_delivery: CaptureDataDelivery::DuringAcquisition,
        capabilities: streaming_capabilities,
        prepare: Box::new(move |context| {
            DeterministicFakeProvider::new(streaming).prepare(context)
        }),
        level_at: Box::new(move |sample, channel| streaming_levels.level_at(sample, channel)),
    });

    let buffered_channels = vec![
        CaptureChannelId::new("pod-a:3"),
        CaptureChannelId::new("pod-q:41"),
        CaptureChannelId::new("aux-bank:9"),
    ];
    let buffered = BufferedFakeConfig::new(buffered_channels.clone(), 2_000_000, 23, 6, 0x31)
        .unwrap()
        .with_simple_trigger(vec![Some(SimpleTriggerCondition::Falling), None, None])
        .unwrap();
    let buffered_trigger = buffered.first_trigger_sample().unwrap();
    let buffered_levels = buffered.clone();
    let buffered_capabilities = buffered.capabilities().clone();
    run_provider_contract(ProviderContractCase {
        name: "buffered fake",
        channels: buffered_channels,
        total_samples: buffered.total_samples(),
        expected_trigger: buffered_trigger,
        expected_delivery: CaptureDataDelivery::BufferedUpload,
        capabilities: buffered_capabilities,
        prepare: Box::new(move |context| BufferedFakeProvider::new(buffered).prepare(context)),
        level_at: Box::new(move |sample, channel| buffered_levels.level_at(sample, channel)),
    });
}

#[test]
fn buffered_provider_commits_nothing_before_upload_begins() {
    let channels = vec![
        CaptureChannelId::new("bank-left:2"),
        CaptureChannelId::new("bank-right:29"),
    ];
    let config = BufferedFakeConfig::new(channels.clone(), 4_000_000, 19, 5, 7).unwrap();
    let session_id = CaptureSessionId::new(0x7133);
    let temporary = tempdir().unwrap();
    let descriptor = CaptureStoreDescriptor::new(session_id, channels).unwrap();
    let (store, writer) = NativeCaptureStore::create(
        NativeCaptureStoreConfig::new(temporary.path(), descriptor)
            .with_commit_batch_chunks(1)
            .unwrap(),
    )
    .unwrap();
    let (events, _event_reader) = bounded_capture_event_queue(64).unwrap();
    let context = AcquisitionContext::new(session_id, Box::new(writer), Box::new(events));
    let (provider, controller) = BufferedFakeProvider::manually_uploaded(config);
    let mut acquisition = provider.prepare(context).unwrap();

    acquisition.start().unwrap();
    assert!(controller.wait_until_upload(TIMEOUT));
    assert_eq!(store.snapshot().committed_samples, 0);
    controller.grant_upload_chunks(1);
    let deadline = Instant::now() + TIMEOUT;
    while store.snapshot().committed_samples == 0 {
        assert!(Instant::now() < deadline, "first upload chunk timed out");
        std::thread::yield_now();
    }
    controller.grant_upload_chunks(8);
    let outcome = acquisition.join().unwrap();
    assert_eq!(outcome.captured_samples, 19);
    assert_eq!(store.snapshot().committed_samples, 19);
}

#[test]
fn buffered_capabilities_use_non_contiguous_ids_and_a_distinct_setting_matrix() {
    let channels = vec![
        CaptureChannelId::new("pod-c:1"),
        CaptureChannelId::new("pod-c:17"),
        CaptureChannelId::new("bank-z:63"),
    ];
    let config = BufferedFakeConfig::new(channels.clone(), 5_000_000, 16, 4, 11).unwrap();
    let capabilities = config.capabilities();
    assert_eq!(
        capabilities.data_delivery(),
        CaptureDataDelivery::BufferedUpload
    );
    assert!(!capabilities.supports_force_trigger());
    assert_eq!(capabilities.setting_matrix().len(), 2);
    assert_eq!(capabilities.setting_matrix()[0].channels(), channels);
    assert_eq!(
        capabilities.setting_matrix()[1].channels(),
        &[channels[0].clone(), channels[2].clone()]
    );
    assert_eq!(
        capabilities.setting_matrix()[1].sample_rates_hz(),
        &[20_000_000]
    );
}
