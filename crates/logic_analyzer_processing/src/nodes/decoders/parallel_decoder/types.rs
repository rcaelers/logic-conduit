/// Strobe signal mode for the parallel decoder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StrobeMode {
    /// Sample on the rising edge of the strobe signal.
    RisingEdge,
    /// Sample on the falling edge of the strobe signal.
    FallingEdge,
    /// Sample on any edge of the strobe signal.
    AnyEdge,
    /// Sample while the strobe signal is high.
    HighLevel,
    /// Sample while the strobe signal is low.
    LowLevel,
}

/// Input transport strategy for the parallel decoder's raw signal inputs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ParallelInputStrategy {
    /// Let protocol negotiation choose. File sources currently prefer an
    /// indexed query when both transports are available.
    #[default]
    Auto,
    /// Consume aligned packed sample blocks.
    PackedStream,
    /// Query strobe edges and data values from a random-access index.
    Indexed,
}
