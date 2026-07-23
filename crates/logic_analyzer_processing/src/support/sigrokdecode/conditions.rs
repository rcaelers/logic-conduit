#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PinCondition {
    High,
    Low,
    Rising,
    Falling,
    EitherEdge,
    NoEdge,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum WaitTerm {
    Pin {
        channel: usize,
        condition: PinCondition,
    },
    Skip(u64),
    Never,
}

impl WaitTerm {
    pub(crate) fn pin(channel: usize, condition: PinCondition) -> Self {
        Self::Pin { channel, condition }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct WaitCondition {
    terms: Vec<WaitTerm>,
}

impl WaitCondition {
    pub(crate) fn new(terms: impl IntoIterator<Item = WaitTerm>) -> Self {
        Self {
            terms: terms.into_iter().collect(),
        }
    }

    pub(crate) fn terms(&self) -> &[WaitTerm] {
        &self.terms
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum WaitRequest {
    Next,
    Conditions(Vec<WaitCondition>),
}
