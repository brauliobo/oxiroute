const GAP: u16 = 1;
const LABEL_FLOOR: u16 = 8;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Candidate {
    pub(crate) index: usize,
    pub(crate) width: u16,
    pub(crate) priority: u8,
    pub(crate) forced: bool,
}

fn span(candidates: &[Candidate]) -> u16 {
    let widths: u16 = candidates.iter().map(|candidate| candidate.width).sum();
    widths + GAP * u16::try_from(candidates.len().saturating_sub(1)).unwrap_or(u16::MAX)
}

fn weakest(candidates: &[Candidate]) -> Option<usize> {
    candidates
        .iter()
        .enumerate()
        .skip(1)
        .max_by_key(|(position, candidate)| (!candidate.forced, candidate.priority, *position))
        .map(|(position, _)| position)
}

pub(crate) fn fit(mut candidates: Vec<Candidate>, available: u16) -> Vec<Candidate> {
    while span(&candidates) > available {
        match weakest(&candidates) {
            Some(position) => {
                candidates.remove(position);
            }
            None => break,
        }
    }
    let over = span(&candidates).saturating_sub(available);
    if let Some(label) = candidates.first_mut() {
        label.width = label
            .width
            .saturating_sub(over)
            .max(LABEL_FLOOR.min(label.width));
    }
    candidates
}
