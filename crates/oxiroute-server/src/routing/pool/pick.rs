#[allow(clippy::wildcard_imports)]
use super::*;

/// Random probes before power-of-two-choices falls back to listing the acceptable servers.
const P2C_PROBES: usize = 6;

impl EndpointPool {
    /// The algorithm's preferred server among the selectable ones that `accept` admits.
    ///
    /// Round robin and first stop at the first acceptable server, power of two choices probes two
    /// random servers, and the load-aware algorithms make one pass; none of them allocates.
    pub(super) fn pick(
        &self,
        selection: &mut SelectionState,
        members: &Membership,
        view: &SelectableView,
        accept: &impl Fn(usize) -> bool,
    ) -> Option<usize> {
        let server = |index: usize| &members.endpoints[index];
        match &self.algorithm {
            UpstreamAlgorithm::RoundRobin => {
                view.rotation(selection.next).find(|&index| accept(index))
            }
            UpstreamAlgorithm::First => view.rotation(0).find(|&index| accept(index)),
            UpstreamAlgorithm::LeastConnections => view
                .rotation(selection.next)
                .filter(|&index| accept(index))
                .min_by_key(|&index| server(index).load()),
            UpstreamAlgorithm::WeightedRoundRobin { .. } => view
                .rotation(0)
                .filter(|&index| accept(index))
                .min_by_key(|&index| {
                    std::cmp::Reverse(selection.credit[index] + i64::from(server(index).weight()))
                }),
            UpstreamAlgorithm::PowerOfTwoChoices => {
                let (first, second) = Self::two_random(selection, view, accept)?;
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
        view: &SelectableView,
        accept: &impl Fn(usize) -> bool,
        chosen: usize,
    ) {
        if matches!(
            &self.algorithm,
            UpstreamAlgorithm::WeightedRoundRobin { .. }
        ) {
            let mut total = 0;
            for index in view.rotation(0).filter(|&index| accept(index)) {
                let weight = i64::from(members.endpoints[index].weight());
                selection.credit[index] += weight;
                total += weight;
            }
            selection.credit[chosen] -= total;
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
        accept: &impl Fn(usize) -> bool,
    ) -> Option<usize> {
        let chosen = self.pick(selection, members, view, accept)?;
        self.commit(selection, members, view, accept, chosen);
        Some(chosen)
    }
}

fn random_below(selection: &mut SelectionState, bound: usize) -> usize {
    usize::try_from(selection.next_random()).unwrap_or(0) % bound
}
