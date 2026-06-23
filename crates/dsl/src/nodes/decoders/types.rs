//! Common decoder types and enums

/// Timing information for decoded events
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TimingInfo {
    /// Timestamp in microseconds
    pub timestamp_us: f64,
    /// Position in capture
    pub position: u64,
}

impl TimingInfo {
    /// Create new timing information
    pub fn new(timestamp_us: f64, position: u64) -> Self {
        Self {
            timestamp_us,
            position,
        }
    }
}

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

/// Decoded SPI transfer
#[derive(Debug, Clone)]
pub struct SpiTransfer {
    /// Data on MOSI line (0 if not configured)
    pub mosi: u32,
    /// Data on MISO line (0 if not configured)
    pub miso: u32,
    /// Timing information
    pub timing: TimingInfo,
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

/// Decoded parallel bus word
#[derive(Debug, Clone)]
pub struct ParallelWord {
    /// Data value (up to 64 bits)
    pub value: u64,
    /// Timing information
    pub timing: TimingInfo,
}
