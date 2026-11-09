//! An index of smooth weighted round robin credits, so the choice does not read every server.
//!
//! The rule: a selection credits every admissible server with its weight, chooses the admissible
//! server with the most credit (the first in membership order among equals) and debits it the total
//! weight credited. A server is admissible by what it can take: a permit (level 2), only an idle
//! pooled connection (level 1) or neither (level 0), and each kind of selection admits the servers
//! of at least some level. The credits are therefore kept as a base per server plus the weight times
//! the selections that admitted its level since, which no selection has to write: a selection
//! counts itself once and debits the chosen server. Servers of one weight and level are ordered by
//! base in an ordered set, so the choice compares one candidate per set, and a server that changes
//! level, which is noted in the pool's [`LoadLog`], moves between sets without losing credit.

use std::{
    cmp::Reverse,
    collections::{BTreeMap, BTreeSet},
};

use super::{LoadLog, Membership, PoolEndpoint, SelectableView, pool::Capacity};

const LEVELS: usize = 3;

/// The members of one class by credit, the one with most credit first, then by position.
type Ranked = BTreeSet<(Reverse<i64>, u32)>;

#[derive(Debug)]
struct Member {
    weight: u16,
    level: u8,
    base: i64,
}

#[derive(Debug, Default)]
pub(super) struct WeightIndex {
    view: u64,
    /// Selections made under each kind of capacity requirement, which credited the servers it admits.
    rounds: [i64; LEVELS],
    /// The selectable servers by membership position.
    members: Vec<Option<Member>>,
    /// The members of each weight and level, the one with most credit first.
    classes: BTreeMap<(u16, u8), Ranked>,
    /// The weight of the members at each level.
    totals: [i64; LEVELS],
    noted: Vec<u32>,
}

fn level_of(server: &PoolEndpoint) -> u8 {
    if server.has_capacity() {
        2
    } else {
        u8::from(server.has_idle_connection())
    }
}

/// Whether a selection requiring `capacity` admits the servers of `level`.
fn admits(capacity: Capacity, level: u8) -> bool {
    usize::from(level) + capacity.kind() >= 2
}

impl WeightIndex {
    /// Credits the servers earned since the base: the selections that admitted their level.
    fn accrued(&self, level: u8) -> i64 {
        self.rounds[2 - usize::from(level)..].iter().sum()
    }

    /// The weight of the servers a selection requiring `capacity` admits.
    fn admitted_weight(&self, capacity: Capacity) -> i64 {
        (0..=2_u8)
            .filter(|&level| admits(capacity, level))
            .map(|level| self.totals[usize::from(level)])
            .sum()
    }

    fn credit_of(&self, member: &Member) -> i64 {
        member.base + i64::from(member.weight) * self.accrued(member.level)
    }

    /// Writes the credits back and forgets the view; the credits are the state again.
    pub(super) fn flush(&mut self, credit: &mut [i64]) {
        if self.view == 0 {
            return;
        }
        for (at, member) in self.members.iter().enumerate() {
            if let Some(member) = member {
                credit[at] = self.credit_of(member);
            }
        }
        *self = Self::default();
    }

    /// Brings the index to `view` and to every level change noted so far.
    pub(super) fn sync(
        &mut self,
        log: &LoadLog,
        members: &Membership,
        view: &SelectableView,
        credit: &mut [i64],
    ) {
        log.track(true);
        let overflowed = log.take(&mut self.noted);
        if self.view == view.id() && !overflowed {
            self.apply_noted(members);
            return;
        }
        self.flush(credit);
        self.view = view.id();
        self.members.resize_with(members.endpoints.len(), || None);
        for entry in &view.entries {
            let at = entry.index as usize;
            let level = level_of(&members.endpoints[at]);
            let member = Member {
                weight: entry.weight,
                level,
                base: credit[at],
            };
            self.totals[usize::from(level)] += i64::from(member.weight);
            self.insert(at, member);
        }
    }

    fn apply_noted(&mut self, members: &Membership) {
        for index in 0..self.noted.len() {
            let at = self.noted[index] as usize;
            let Some(server) = members.endpoints.get(at) else {
                continue;
            };
            let level = level_of(server);
            if self.members[at]
                .as_ref()
                .is_some_and(|member| member.level != level)
            {
                self.change_level(at, level);
            }
        }
    }

    fn insert(&mut self, at: usize, member: Member) {
        let key = (member.weight, member.level);
        let position = (
            Reverse(member.base),
            u32::try_from(at).expect("pool size fits u32"),
        );
        self.classes.entry(key).or_default().insert(position);
        self.members[at] = Some(member);
    }

    fn take(&mut self, at: usize) -> Member {
        let member = self.members[at].take().expect("indexed member");
        let position = (
            Reverse(member.base),
            u32::try_from(at).expect("pool size fits u32"),
        );
        self.classes
            .get_mut(&(member.weight, member.level))
            .expect("class of the member")
            .remove(&position);
        member
    }

    fn change_level(&mut self, at: usize, level: u8) {
        let mut member = self.take(at);
        let weight = i64::from(member.weight);
        member.base += weight * (self.accrued(member.level) - self.accrued(level));
        self.totals[usize::from(member.level)] -= weight;
        self.totals[usize::from(level)] += weight;
        member.level = level;
        self.insert(at, member);
    }

    fn debit(&mut self, at: usize, debit: i64) {
        let mut member = self.take(at);
        member.base -= debit;
        self.insert(at, member);
    }

    /// The admitted member with most credit once credited, outside `masked`.
    pub(super) fn best(&self, capacity: Capacity, masked: &[usize]) -> Option<usize> {
        let mut best = None;
        for (&(weight, level), set) in &self.classes {
            if !admits(capacity, level) {
                continue;
            }
            let top = set.iter().find(|(_, at)| !masked.contains(&(*at as usize)));
            let Some(&(Reverse(base), at)) = top else {
                continue;
            };
            let credit = base + i64::from(weight) * (self.accrued(level) + 1);
            let candidate = (credit, Reverse(at));
            if best.is_none_or(|best| candidate > best) {
                best = Some(candidate);
            }
        }
        best.map(|(_, Reverse(at))| at as usize)
    }

    /// Records a selection of `chosen` among the servers `capacity` admits except `excluded`.
    pub(super) fn select(&mut self, capacity: Capacity, chosen: usize, excluded: &[usize]) {
        let mut skipped = excluded
            .iter()
            .copied()
            .filter(|&at| {
                self.members
                    .get(at)
                    .and_then(Option::as_ref)
                    .is_some_and(|member| admits(capacity, member.level))
            })
            .collect::<Vec<_>>();
        skipped.sort_unstable();
        skipped.dedup();
        let skipped_weight = skipped
            .iter()
            .filter_map(|&at| self.members[at].as_ref())
            .map(|member| i64::from(member.weight))
            .sum::<i64>();
        let total = self.admitted_weight(capacity) - skipped_weight;
        self.rounds[capacity.kind()] += 1;
        for at in skipped {
            let weight = self.members[at].as_ref().map_or(0, |m| i64::from(m.weight));
            self.debit(at, weight);
        }
        self.debit(chosen, total);
    }
}
