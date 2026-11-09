use std::cmp::Reverse;

#[allow(clippy::wildcard_imports)]
use super::*;

/// Random probes before power-of-two-choices falls back to listing the acceptable servers.
const P2C_PROBES: usize = 6;

/// Up to this many selectable servers least connections and weighted round robin read every server
/// in one pass, which costs less than keeping an index current.
const SCANNED: usize = 8;

impl EndpointPool {
    /// The algorithm's preferred server among the selectable ones that `accept` admits.
    ///
    /// Round robin and first stop at the first acceptable server and power of two choices probes two
    /// random servers. Least connections and weighted round robin make one pass over a few servers
    /// and consult an index of many; none of them allocates unless a retry excludes servers.
    pub(super) fn pick(
        &self,
        selection: &mut SelectionState,
        members: &Membership,
        view: &SelectableView,
        eligible: &Eligibility,
    ) -> Option<usize> {
        let server = |index: usize| &members.endpoints[index];
        let accept = |index: usize| eligible.admits(members, index);
        match &self.algorithm {
            UpstreamAlgorithm::RoundRobin => {
                view.rotation(selection.next).find(|&index| accept(index))
            }
            UpstreamAlgorithm::First => view.rotation(0).find(|&index| accept(index)),
            UpstreamAlgorithm::LeastConnections if view.entries.len() <= SCANNED => {
                self.queue.load_log.track(false);
                Self::least_loaded(view.rotation(selection.next), members, eligible)
            }
            UpstreamAlgorithm::LeastConnections => {
                let start = selection.next;
                let masked = eligible.masked(members);
                selection
                    .loads
                    .tree(&self.queue.load_log, members, view, eligible.capacity)
                    .least(start, &masked)
            }
            UpstreamAlgorithm::WeightedRoundRobin { .. } if view.entries.len() <= SCANNED => {
                self.queue.load_log.track(false);
                selection.weights.flush(&mut selection.credit);
                view.entries
                    .iter()
                    .filter(|entry| accept(entry.index as usize))
                    .min_by_key(|entry| {
                        Reverse(selection.credit[entry.index as usize] + i64::from(entry.weight))
                    })
                    .map(|entry| entry.index as usize)
            }
            UpstreamAlgorithm::WeightedRoundRobin { .. } => {
                let masked = eligible.masked(members);
                selection
                    .weights
                    .sync(&self.queue.load_log, members, view, &mut selection.credit);
                selection.weights.best(eligible.capacity, &masked)
            }
            UpstreamAlgorithm::PowerOfTwoChoices => {
                let (first, second) = Self::two_random(selection, view, &accept)?;
                Some(match second {
                    Some(second) if server(second).load() < server(first).load() => second,
                    _ => first,
                })
            }
        }
    }

    /// The eligible server with the least load, the first in `rotation` among equals. A server
    /// without work cannot be beaten, so the scan stops at the first one.
    fn least_loaded(
        rotation: impl Iterator<Item = usize>,
        members: &Membership,
        eligible: &Eligibility,
    ) -> Option<usize> {
        let mut best: Option<(u64, usize)> = None;
        for index in rotation {
            let Some(load) = eligible.load(members, index) else {
                continue;
            };
            if best.is_none_or(|(least, _)| load < least) {
                best = Some((load, index));
                if load == 0 {
                    break;
                }
            }
        }
        best.map(|(_, index)| index)
    }

    /// Two distinct acceptable servers chosen uniformly, or the only one there is.
    fn two_random(
        selection: &mut SelectionState,
        view: &SelectableView,
        accept: &impl Fn(usize) -> bool,
    ) -> Option<(usize, Option<usize>)> {
        let entries = &view.entries;
        let mut picked: [Option<usize>; 2] = [None; 2];
        for _ in 0..P2C_PROBES {
            let index = entries[random_below(selection, entries.len())].index as usize;
            if picked[0] != Some(index) && accept(index) {
                picked[usize::from(picked[0].is_some())] = Some(index);
                if picked[1].is_some() {
                    break;
                }
            }
        }
        if let [Some(first), Some(second)] = picked {
            return Some((first, Some(second)));
        }
        let acceptable = view
            .rotation(0)
            .filter(|&index| accept(index))
            .collect::<Vec<_>>();
        let len = acceptable.len();
        if len == 0 {
            return None;
        }
        let first = random_below(selection, len);
        let second = (len > 1).then(|| {
            let second = random_below(selection, len - 1);
            acceptable[second + usize::from(second >= first)]
        });
        Some((acceptable[first], second))
    }

    /// Records that `chosen` was picked among the servers `accept` admits.
    pub(super) fn commit(
        &self,
        selection: &mut SelectionState,
        members: &Membership,
        view: &SelectableView,
        eligible: &Eligibility,
        chosen: usize,
    ) {
        if matches!(
            &self.algorithm,
            UpstreamAlgorithm::WeightedRoundRobin { .. }
        ) {
            if view.entries.len() <= SCANNED {
                let mut total = 0;
                for entry in &view.entries {
                    if eligible.admits(members, entry.index as usize) {
                        let weight = i64::from(entry.weight);
                        selection.credit[entry.index as usize] += weight;
                        total += weight;
                    }
                }
                selection.credit[chosen] -= total;
            } else {
                let excluded = eligible.excluded_positions(members);
                selection
                    .weights
                    .select(eligible.capacity, chosen, &excluded);
            }
        }
        if !matches!(&self.algorithm, UpstreamAlgorithm::First) {
            selection.next = (chosen + 1) % members.endpoints.len();
        }
    }

    /// Picks and commits in one step, for selections that do not acquire a permit.
    pub(super) fn take(
        &self,
        selection: &mut SelectionState,
        members: &Membership,
        view: &SelectableView,
        eligible: &Eligibility,
    ) -> Option<usize> {
        let chosen = self.pick(selection, members, view, eligible)?;
        self.commit(selection, members, view, eligible, chosen);
        Some(chosen)
    }
}

fn random_below(selection: &mut SelectionState, bound: usize) -> usize {
    usize::try_from(selection.next_random()).unwrap_or(0) % bound
}
