// Sequential parallel-decoder execution used by wasm.

#[derive(Default)]
pub(crate) struct ParallelStreamState;

fn platform_effective_workers(_requested: usize, _metrics: &ParallelDecoderMetrics) -> usize {
    1
}

fn work_with_platform_backend(
    decoder: &mut ParallelDecoder,
    inputs: &[InputPort],
    outputs: &[OutputPort],
    blocks: &mut StreamBlockState,
) -> WorkResult<usize> {
    let _state = &blocks.parallel;
    decoder.work_streamed_inner(inputs, outputs, blocks)
}
