#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum Tier {
    Minimal,
    Compact,
    Standard,
    Wide,
}

impl Tier {
    pub(crate) const fn of(width: u16) -> Self {
        match width {
            120.. => Self::Wide,
            100..=119 => Self::Standard,
            80..=99 => Self::Compact,
            _ => Self::Minimal,
        }
    }
}
