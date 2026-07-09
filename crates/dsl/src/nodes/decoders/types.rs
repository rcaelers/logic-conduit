//! Common decoder types and enums

/// SPI clock polarity and phase modes
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpiMode {
    /// CPOL=0, CPHA=0: Clock idle low, sample on rising edge
    Mode0,
    /// CPOL=0, CPHA=1: Clock idle low, sample on falling edge
    Mode1,
    /// CPOL=1, CPHA=0: Clock idle high, sample on falling edge
    Mode2,
    /// CPOL=1, CPHA=1: Clock idle high, sample on rising edge
    Mode3,
}

/// Chip select polarity for decoders
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CsPolarity {
    /// CS is active-low (standard SPI): LOW = active, HIGH = inactive
    ActiveLow,
    /// CS is active-high: HIGH = active, LOW = inactive
    ActiveHigh,
    /// CS state is ignored (decoder always considers CS as inactive/enabled)
    Disabled,
}

/// Bit order for serial decoders
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BitOrder {
    /// Most significant bit first (standard SPI)
    #[default]
    MsbFirst,
    /// Least significant bit first
    LsbFirst,
}

/// Byte/cycle order when assembling multi-cycle words
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Endianness {
    /// First cycle is the least-significant part
    #[default]
    Little,
    /// First cycle is the most-significant part
    Big,
}

/// Strobe signal mode for parallel decoder
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StrobeMode {
    /// Sample on rising edge of strobe signal
    RisingEdge,
    /// Sample on falling edge of strobe signal
    FallingEdge,
    /// Sample on any edge of strobe signal
    AnyEdge,
    /// Sample when strobe is high (level-triggered)
    HighLevel,
    /// Sample when strobe is low (level-triggered)
    LowLevel,
}
