/// Chip-select polarity used by digital decoders.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CsPolarity {
    /// Chip select is active low.
    ActiveLow,
    /// Chip select is active high.
    ActiveHigh,
    /// Chip-select state is ignored.
    Disabled,
}

/// Bit order used when assembling serial values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BitOrder {
    /// Most-significant bit first.
    #[default]
    MsbFirst,
    /// Least-significant bit first.
    LsbFirst,
}

/// Byte or cycle order used when assembling multi-cycle words.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Endianness {
    /// The first cycle is the least-significant part.
    #[default]
    Little,
    /// The first cycle is the most-significant part.
    Big,
}
