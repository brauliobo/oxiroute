#[allow(clippy::wildcard_imports)]
use super::*;

/// Random probes before power-of-two-choices falls back to listing the acceptable servers.
const P2C_PROBES: usize = 6;

impl EndpointPool {
    /// The algorithm's preferred server among the selectable ones that `accept` admits.
    ///
    /// Round robin and first stop at the first acceptable server, power of two choices probes two
    /// random servers, least connections reads the load index, and weighted round robin makes one
    /// pass; none of them allocates unless a retry excludes servers.
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
            UpstreamAlgorithm::LeastConnections => {
                let start = selection.next;
                let masked = eligible.masked(members);
                selection
                    .loads
                    .tree(&self.queue.load_log, members, view, eligible.capacity)
                    .least(start, &masked)
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
        eligible: &Eligibility,
        chosen: usize,
    ) {
        if matches!(
            &self.algorithm,
            UpstreamAlgorithm::WeightedRoundRobin { .. }
        ) {
            let excluded = eligible.excluded_positions(members);
            selection
                .weights
                .select(eligible.capacity, chosen, &excluded);
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
        self.commit(selection, members, eligible, chosen);
        Some(chosen)
    }
}

fn random_below(selection: &mut SelectionState, bound: usize) -> usize {
    usize::try_from(selection.next_random()).unwrap_or(0) % bound
}
