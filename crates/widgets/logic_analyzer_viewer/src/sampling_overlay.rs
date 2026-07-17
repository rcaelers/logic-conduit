/// Which clock transitions are sampling instants for a clocked consumer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SamplingEdge {
    Rising,
    Falling,
    Both,
}

impl SamplingEdge {
    pub(crate) fn accepts(self, value_after_edge: bool) -> bool {
        match self {
            Self::Rising => value_after_edge,
            Self::Falling => !value_after_edge,
            Self::Both => true,
        }
    }
}

/// Protocol-neutral description of sampling markers drawn over raw capture
/// channels. Channel numbers are the stable indices used by the capture
/// presented to the viewer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SamplingOverlay {
    pub clock_channel: usize,
    pub sampled_channels: Vec<usize>,
    pub edge: SamplingEdge,
}
