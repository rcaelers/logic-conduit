//! Sequential parallel-decoder execution used by wasm.

use super::*;

#[derive(Default)]
pub(super) struct ParallelStreamState;

pub(super) fn effective_workers(_requested: usize, _metrics: &ParallelDecoderMetrics) -> usize {
    1
}

pub(super) fn work(
    decoder: &mut ParallelDecoder,
    inputs: &[InputPort],
    outputs: &[OutputPort],
    blocks: &mut StreamBlockState,
) -> WorkResult<usize> {
    let _state = &blocks.parallel;
    decoder.work_streamed_inner(inputs, outputs, blocks)
}
