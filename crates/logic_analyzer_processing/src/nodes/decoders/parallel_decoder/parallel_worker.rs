// Native parallel-decoder execution using the shared worker pool when requested.

enum FragmentCompletion {
    Complete(DecodeFragment),
    Panicked(u64),
}

#[derive(Default)]
pub(crate) struct ParallelStreamState {
    completion_sender: Option<crossbeam_channel::Sender<FragmentCompletion>>,
    completion_receiver: Option<crossbeam_channel::Receiver<FragmentCompletion>>,
    in_flight: usize,
    input_exhausted: bool,
    reorder: BTreeMap<u64, DecodeFragment>,
    available_buffers: Vec<DecodeFragmentBuffers>,
}

fn platform_effective_workers(requested: usize, metrics: &ParallelDecoderMetrics) -> usize {
    let workers = requested.min(signal_processing::shared_worker_pool().workers());
    metrics.inner.workers.store(workers, Ordering::Relaxed);
    workers
}

fn work_with_platform_backend(
    decoder: &mut ParallelDecoder,
    inputs: &[InputPort],
    outputs: &[OutputPort],
    blocks: &mut StreamBlockState,
) -> WorkResult<usize> {
    let workers = if decoder.parallel_workers > 1 {
        platform_effective_workers(decoder.parallel_workers, &decoder.parallel_metrics)
    } else {
        1
    };
    if workers > 1 {
        work_parallel(decoder, inputs, outputs, blocks, workers)
    } else {
        decoder.work_streamed_inner(inputs, outputs, blocks)
    }
}

fn fragment_buffer_bytes(buffers: &DecodeFragmentBuffers) -> usize {
    buffers
        .positions
        .capacity()
        .saturating_mul(std::mem::size_of::<u64>())
        .saturating_add(
            buffers
                .values
                .capacity()
                .saturating_mul(std::mem::size_of::<u64>()),
        )
        .saturating_add(buffers.reset_before.capacity().div_ceil(u8::BITS as usize))
}

fn receive_next_fragment(
    blocks: &mut StreamBlockState,
    expected_sequence: u64,
    metrics: &ParallelDecoderMetrics,
) -> WorkResult<DecodeFragment> {
    if let Some(fragment) = blocks.parallel.reorder.remove(&expected_sequence) {
        return Ok(fragment);
    }

    loop {
        if blocks.parallel.in_flight == 0 {
            if blocks.parallel.input_exhausted {
                return Err(WorkError::Shutdown);
            }
            return Err(WorkError::NodeError(
                "Parallel decoder has no outstanding fragment".to_string(),
            ));
        }
        let completion = blocks
            .parallel
            .completion_receiver
            .as_ref()
            .ok_or_else(|| {
                WorkError::NodeError("Fragment completion channel is missing".to_string())
            })?
            .recv()
            .map_err(|_| WorkError::NodeError("Fragment completion channel closed".to_string()))?;
        blocks.parallel.in_flight -= 1;
        let fragment = match completion {
            FragmentCompletion::Complete(fragment) => fragment,
            FragmentCompletion::Panicked(sequence) => {
                return Err(WorkError::NodeError(format!(
                    "Fragment worker panicked while scanning sequence {sequence}"
                )));
            }
        };
        metrics
            .inner
            .max_fragment_bytes
            .fetch_max(fragment_buffer_bytes(&fragment.buffers), Ordering::Relaxed);
        if fragment.sequence == expected_sequence {
            return Ok(fragment);
        }
        blocks.parallel.reorder.insert(fragment.sequence, fragment);
        metrics
            .inner
            .max_reorder
            .fetch_max(blocks.parallel.reorder.len(), Ordering::Relaxed);
    }
}

fn work_parallel(
    decoder: &mut ParallelDecoder,
    inputs: &[InputPort],
    outputs: &[OutputPort],
    blocks: &mut StreamBlockState,
    workers: usize,
) -> WorkResult<usize> {
    decoder.work_call_count += 1;
    let output = outputs.first().and_then(|port| port.get::<Word>());

    let mut strobe_buf = VecDeque::new();
    let mut strobe_input = inputs
        .first()
        .and_then(|port| port.get::<SampleBlock>(&mut strobe_buf))
        .ok_or_else(|| WorkError::NodeError("Missing strobe block input".to_string()))?;
    let mut data_bufs: Vec<VecDeque<SampleBlock>> = (0..decoder.num_data_bits)
        .map(|_| VecDeque::new())
        .collect();
    let mut data_inputs: Vec<Receiver<'_, SampleBlock>> = Vec::with_capacity(decoder.num_data_bits);
    for (index, buffer) in data_bufs.iter_mut().enumerate() {
        let input = inputs
            .get(1 + index)
            .and_then(|port| port.get::<SampleBlock>(buffer))
            .ok_or_else(|| WorkError::NodeError(format!("Missing data block input {index}")))?;
        data_inputs.push(input);
    }

    let mut cs_buf = VecDeque::new();
    let mut cs_input = inputs
        .get(1 + decoder.num_data_bits)
        .and_then(|port| port.get::<SampleBlock>(&mut cs_buf));
    if cs_input.is_none() && decoder.cs_polarity != CsPolarity::Disabled {
        return Err(WorkError::NodeError(
            "CS input unconnected but CS polarity is not Disabled".to_string(),
        ));
    }

    let enable_port_idx = 1 + decoder.num_data_bits + 1;
    let enable_query = inputs
        .get(enable_port_idx)
        .and_then(|port| port.edge_query());
    let mut current_enable_value = decoder.current_enable_value;
    let mut enable_input = if enable_query.is_some() {
        None
    } else {
        inputs
            .get(enable_port_idx)
            .and_then(|port| port.get::<Sample>(&mut decoder.enable_buffer))
    };
    if enable_query.is_none() && enable_input.is_none() {
        current_enable_value = true;
    }

    let max_outstanding = workers * 2;
    if blocks.parallel.completion_sender.is_none() {
        let (sender, receiver) = crossbeam_channel::bounded(max_outstanding);
        blocks.parallel.completion_sender = Some(sender);
        blocks.parallel.completion_receiver = Some(receiver);
    }

    while blocks.parallel.in_flight + blocks.parallel.reorder.len() < max_outstanding
        && !blocks.parallel.input_exhausted
    {
        if blocks.strobe.is_none() {
            match acquire_stream_block_set(
                &mut strobe_input,
                &mut data_inputs,
                &mut cs_input,
                blocks,
            ) {
                Ok(()) => {}
                Err(WorkError::Shutdown) => {
                    blocks.parallel.input_exhausted = true;
                    break;
                }
                Err(error) => return Err(error),
            }
        }

        let strobe = blocks
            .strobe
            .as_ref()
            .expect("parallel block set acquired above");
        let window_start = blocks.offset;
        let window_end = window_start
            .saturating_add(ParallelDecoder::STREAM_SAMPLES_PER_CALL)
            .min(strobe.num_samples);
        let block_samples = strobe.num_samples;
        let sequence = blocks.next_sequence;
        let enabled_ranges = enabled_ranges_for_window(
            enable_query.as_ref(),
            &mut enable_input,
            &mut current_enable_value,
            strobe.start_position,
            window_start,
            window_end,
            strobe.timestamp_step,
        )?;
        record_enabled_ranges(
            decoder.enable_activity.as_ref(),
            strobe.start_position,
            strobe.timestamp_step,
            &enabled_ranges,
        );
        let strobe = strobe.clone();
        let data = blocks.data.clone();
        let cs = blocks.cs.clone();
        let config = StreamScanConfig {
            mode: decoder.mode,
            cs_polarity: decoder.cs_polarity,
        };
        let buffers = blocks.parallel.available_buffers.pop().unwrap_or_default();
        let completion = blocks
            .parallel
            .completion_sender
            .as_ref()
            .expect("completion channel initialized above")
            .clone();
        signal_processing::shared_worker_pool()
            .spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    scan_stream_fragment(
                        config,
                        sequence,
                        &strobe,
                        &data,
                        cs.as_ref(),
                        window_start,
                        window_end,
                        &enabled_ranges,
                        buffers,
                    )
                }));
                let result = match result {
                    Ok(fragment) => FragmentCompletion::Complete(fragment),
                    Err(_) => FragmentCompletion::Panicked(sequence),
                };
                let _ = completion.send(result);
            })
            .map_err(|_| WorkError::NodeError("Shared fragment worker pool stopped".to_string()))?;

        blocks.parallel.in_flight += 1;
        blocks.next_sequence += 1;
        decoder.parallel_metrics.inner.max_outstanding.fetch_max(
            blocks.parallel.in_flight + blocks.parallel.reorder.len(),
            Ordering::Relaxed,
        );
        blocks.offset = window_end;
        if window_end == block_samples {
            blocks.strobe = None;
            blocks.data.clear();
            blocks.cs = None;
            blocks.offset = 0;
        }
    }

    let mut fragment = receive_next_fragment(
        blocks,
        decoder.next_stream_merge_sequence,
        &decoder.parallel_metrics,
    )?;
    if fragment.sequence != decoder.next_stream_merge_sequence {
        return Err(WorkError::NodeError(format!(
            "Out-of-order decode fragment: expected sequence {}, received {}",
            decoder.next_stream_merge_sequence, fragment.sequence
        )));
    }

    let mut word_batch = output
        .as_ref()
        .map(|_| Vec::with_capacity(fragment.buffers.positions.len()));
    let mut assembly = AssemblyState {
        value: decoder.assembly_value,
        cycles: decoder.assembly_cycles,
        first_ts: decoder.assembly_first_ts,
    };
    let mut last_strobe_value = decoder.last_strobe_value;
    let words_emitted = merge_stream_fragment(
        &fragment,
        decoder.mode,
        &mut last_strobe_value,
        decoder.num_data_bits,
        decoder.cycles_per_word,
        decoder.endianness,
        &mut assembly,
        &mut word_batch,
    )?;

    decoder.next_stream_merge_sequence += 1;
    decoder.last_strobe_value = last_strobe_value;
    decoder.current_enable_value = current_enable_value;
    decoder.total_words_emitted += words_emitted;
    decoder.assembly_value = assembly.value;
    decoder.assembly_cycles = assembly.cycles;
    decoder.assembly_first_ts = assembly.first_ts;
    blocks
        .parallel
        .available_buffers
        .push(std::mem::take(&mut fragment.buffers));

    if let (Some(output), Some(batch)) = (&output, word_batch)
        && !batch.is_empty()
    {
        output.send_batch(batch)?;
    }

    debug!(
        "[{}] Parallel stream fragment {} done: {} words, {} in flight, {} reordered",
        decoder.name,
        fragment.sequence,
        words_emitted,
        blocks.parallel.in_flight,
        blocks.parallel.reorder.len()
    );
    Ok(words_emitted as usize)
}

#[cfg(test)]
mod parallel_worker_tests {
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use crossbeam_channel::bounded;

    use signal_processing::ProcessNode;
    use signal_processing::Scheduler;
    use signal_processing::{ChannelMessage, Sender};
    use signal_processing::Watchdog;

    use super::*;

    fn block_from_bits(bits: &[bool]) -> SampleBlock {
        let mut bytes = vec![0u8; bits.len().div_ceil(8)];
        for (index, &bit) in bits.iter().enumerate() {
            if bit {
                bytes[index / 8] |= 1 << (index % 8);
            }
        }
        SampleBlock::new(Arc::from(bytes.into_boxed_slice()), 0, bits.len(), 1)
    }

    fn block_input(watchdog: &Watchdog, block: SampleBlock, name: &str) -> InputPort {
        let (sender, receiver) = bounded::<ChannelMessage<SampleBlock>>(4);
        sender.send(ChannelMessage::Sample(block)).unwrap();
        drop(sender);
        InputPort::new_with_watchdog(receiver, watchdog, "pd", name)
    }

    fn unconnected(_watchdog: &Watchdog, _name: &str) -> InputPort {
        InputPort::disconnected()
    }

    fn collect_messages<T>(receiver: crossbeam_channel::Receiver<ChannelMessage<T>>) -> Vec<T> {
        let mut collected = Vec::new();
        for message in receiver.try_iter() {
            match message {
                ChannelMessage::Sample(item) => collected.push(item),
                ChannelMessage::Batch(items) => collected.extend(items),
                ChannelMessage::EndOfStream => {}
            }
        }
        collected
    }

    #[test]
    fn completion_reorders_fragments_by_sequence() {
        let strobe = block_from_bits(&[false, true, false, true, false, true, false, true]);
        let data = [block_from_bits(&[true; 8])];
        let config = StreamScanConfig {
            mode: StrobeMode::RisingEdge,
            cs_polarity: CsPolarity::Disabled,
        };
        let scan = |sequence, start, end| {
            scan_stream_fragment(
                config,
                sequence,
                &strobe,
                &data,
                None,
                start,
                end,
                &[EnabledRange {
                    start,
                    end,
                    reset_before: false,
                }],
                DecodeFragmentBuffers::default(),
            )
        };
        let first = scan(0, 0, 4);
        let second = scan(1, 4, 8);
        let (sender, receiver) = bounded(2);
        sender.send(FragmentCompletion::Complete(second)).unwrap();
        sender.send(FragmentCompletion::Complete(first)).unwrap();
        let mut blocks = StreamBlockState {
            parallel: ParallelStreamState {
                completion_receiver: Some(receiver),
                in_flight: 2,
                input_exhausted: true,
                ..ParallelStreamState::default()
            },
            ..StreamBlockState::default()
        };
        let metrics = ParallelDecoderMetrics::default();

        let first = receive_next_fragment(&mut blocks, 0, &metrics).unwrap();
        assert_eq!(first.sequence, 0);
        assert_eq!(blocks.parallel.reorder.len(), 1);
        assert_eq!(metrics.snapshot().max_reorder, 1);

        let second = receive_next_fragment(&mut blocks, 1, &metrics).unwrap();
        assert_eq!(second.sequence, 1);
        assert!(blocks.parallel.reorder.is_empty());
        assert_eq!(blocks.parallel.in_flight, 0);
    }

    fn run_multi_window_stream(workers: usize) -> Vec<Word> {
        let watchdog = Watchdog::new();
        let sample_count = 2 * ParallelDecoder::STREAM_SAMPLES_PER_CALL + 17;
        let strobe: Vec<bool> = (0..sample_count)
            .map(|position| position % 4 == 1 || position % 4 == 2)
            .collect();
        let values: Vec<u64> = (0..sample_count)
            .map(|position| ((position / 4) & 0xf) as u64)
            .collect();
        let mut inputs = vec![block_input(&watchdog, block_from_bits(&strobe), "strobe")];
        for bit in 0..4 {
            let data: Vec<bool> = values.iter().map(|value| value & (1 << bit) != 0).collect();
            inputs.push(block_input(
                &watchdog,
                block_from_bits(&data),
                &format!("d{bit}"),
            ));
        }
        inputs.push(unconnected(&watchdog, "cs"));
        inputs.push(unconnected(&watchdog, "enable_signal"));

        let (sender, receiver) = bounded::<ChannelMessage<Word>>(128);
        let outputs = [OutputPort::new_with_watchdog(
            Sender::new(vec![sender]),
            &watchdog,
            "pd",
            "words",
        )];
        let mut decoder = ParallelDecoder::new(4, StrobeMode::RisingEdge, CsPolarity::Disabled)
            .with_word_assembly(3, Endianness::Little)
            .with_parallel_workers(workers);

        loop {
            match decoder.work(&inputs, &outputs) {
                Ok(_) => {}
                Err(WorkError::Shutdown) => break,
                Err(error) => panic!("unexpected error: {error}"),
            }
        }
        collect_messages(receiver)
    }

    #[test]
    fn stream_matches_sequential_across_window_boundaries() {
        let sequential = run_multi_window_stream(1);
        let parallel = run_multi_window_stream(4);

        assert_eq!(parallel, sequential);
        assert!(parallel.len() > 10_000);
    }

    #[test]
    fn queued_scan_stops_within_latency_budget() {
        let sample_count = ParallelDecoder::STREAM_SAMPLES_PER_CALL * 1_024;
        let byte_count = sample_count.div_ceil(u8::BITS as usize);
        let strobe = SampleBlock::new(
            Arc::<[u8]>::from(vec![0xaa; byte_count].into_boxed_slice()),
            0,
            sample_count,
            1,
        );
        let data = SampleBlock::new(
            Arc::<[u8]>::from(vec![0u8; byte_count].into_boxed_slice()),
            0,
            sample_count,
            1,
        );

        let mut scheduler = Scheduler::new();
        let inputs = vec![
            block_input(scheduler.watchdog(), strobe, "strobe"),
            block_input(scheduler.watchdog(), data, "d0"),
            unconnected(scheduler.watchdog(), "cs"),
            unconnected(scheduler.watchdog(), "enable_signal"),
        ];
        let decoder = ParallelDecoder::new(1, StrobeMode::AnyEdge, CsPolarity::Disabled)
            .with_input_strategy(ParallelInputStrategy::PackedStream)
            .with_parallel_workers(4);
        let metrics = decoder.parallel_metrics();
        scheduler.start_process(Box::new(decoder), inputs, Vec::new());

        let dispatch_deadline = Instant::now() + Duration::from_secs(1);
        while metrics.snapshot().max_outstanding == 0 && Instant::now() < dispatch_deadline {
            std::thread::yield_now();
        }
        assert!(
            metrics.snapshot().max_outstanding > 0,
            "decoder did not dispatch parallel work"
        );

        let stop = scheduler.stop_handle();
        let started = Instant::now();
        stop.stop();
        scheduler.wait();
        let elapsed = started.elapsed();
        assert!(
            elapsed < Duration::from_millis(100),
            "parallel decoder stop took {elapsed:?}"
        );
    }
}
