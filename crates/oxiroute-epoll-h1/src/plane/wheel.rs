use std::time::Duration;

use super::clock::Moment;

const BUCKETS: usize = 4096;
const MASK: u64 = BUCKETS as u64 - 1;
const NONE: u32 = u32::MAX;
const WORDS: usize = BUCKETS / 64;

#[derive(Clone, Copy)]
struct Node {
    next: u32,
    prev: u32,
    due: u64,
    armed: bool,
}

const UNARMED: Node = Node {
    next: NONE,
    prev: NONE,
    due: 0,
    armed: false,
};

pub(crate) struct Wheel {
    tick_ns: u64,
    cursor: u64,
    heads: Box<[u32; BUCKETS]>,
    occupied: [u64; WORDS],
    nodes: Vec<Node>,
}

impl Wheel {
    pub(crate) fn new(tick: Duration, capacity: usize) -> Self {
        Self {
            tick_ns: u64::try_from(tick.as_nanos()).unwrap_or(u64::MAX).max(1),
            cursor: 0,
            heads: Box::new([NONE; BUCKETS]),
            occupied: [0; WORDS],
            nodes: vec![UNARMED; capacity],
        }
    }

    fn tick_of(&self, at: Moment) -> u64 {
        at.nanos() / self.tick_ns
    }

    fn ceil_tick_of(&self, at: Moment) -> u64 {
        at.nanos().div_ceil(self.tick_ns)
    }

    fn fires_after(&self, due: u64, deadline: Moment) -> bool {
        due > 0 && (due - 1).saturating_mul(self.tick_ns) >= deadline.nanos()
    }

    pub(crate) fn armed_due(&self, id: u32) -> Option<u64> {
        let node = self.nodes[id as usize];
        node.armed.then_some(node.due)
    }

    pub(crate) fn ensure(&mut self, id: u32, deadline: Moment) {
        if self
            .armed_due(id)
            .is_none_or(|due| self.fires_after(due, deadline))
        {
            self.schedule(id, deadline);
        }
    }

    pub(crate) fn schedule(&mut self, id: u32, deadline: Moment) {
        self.cancel(id);
        let due = self
            .ceil_tick_of(deadline)
            .clamp(self.cursor, self.cursor + MASK);
        let bucket = (due & MASK) as usize;
        let head = self.heads[bucket];
        self.nodes[id as usize] = Node {
            next: head,
            prev: NONE,
            due,
            armed: true,
        };
        if head != NONE {
            self.nodes[head as usize].prev = id;
        }
        self.heads[bucket] = id;
        self.occupied[bucket / 64] |= 1 << (bucket % 64);
    }

    pub(crate) fn cancel(&mut self, id: u32) {
        let node = self.nodes[id as usize];
        if !node.armed {
            return;
        }
        let bucket = (node.due & MASK) as usize;
        if node.prev == NONE {
            self.heads[bucket] = node.next;
        } else {
            self.nodes[node.prev as usize].next = node.next;
        }
        if node.next != NONE {
            self.nodes[node.next as usize].prev = node.prev;
        }
        if self.heads[bucket] == NONE {
            self.occupied[bucket / 64] &= !(1 << (bucket % 64));
        }
        self.nodes[id as usize] = UNARMED;
    }

    pub(crate) fn advance(&mut self, now: Moment, expired: &mut Vec<u32>) {
        let now_tick = self.tick_of(now);
        if now_tick < self.cursor {
            return;
        }
        let span = (now_tick - self.cursor + 1).min(BUCKETS as u64);
        for step in 0..span {
            self.drain_bucket(((self.cursor + step) & MASK) as usize, now_tick, expired);
        }
        self.cursor = now_tick + 1;
    }

    fn drain_bucket(&mut self, bucket: usize, now_tick: u64, expired: &mut Vec<u32>) {
        let mut id = self.heads[bucket];
        while id != NONE {
            let node = self.nodes[id as usize];
            if node.due <= now_tick {
                self.cancel(id);
                expired.push(id);
            }
            id = node.next;
        }
    }

    pub(crate) fn next_wake(&self, now: Moment) -> Option<Duration> {
        let start = (self.cursor & MASK) as usize;
        let distance = self.first_occupied_from(start)?;
        let due = self.cursor + distance as u64;
        let wake = Moment::from_nanos(due.saturating_mul(self.tick_ns));
        Some(wake.since(now))
    }

    fn first_occupied_from(&self, start: usize) -> Option<usize> {
        (0..=WORDS).find_map(|step| {
            let word = (start / 64 + step) % WORDS;
            let mut bits = self.occupied[word];
            if step == 0 {
                bits &= !0_u64 << (start % 64);
            } else if step == WORDS {
                bits &= !(!0_u64 << (start % 64));
            }
            (bits != 0).then(|| {
                let bucket = word * 64 + bits.trailing_zeros() as usize;
                (bucket + BUCKETS - start) % BUCKETS
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plane::clock::Moment;

    const TICK: Duration = Duration::from_millis(10);

    fn at(ticks: u32) -> Moment {
        Moment::from_nanos(u64::try_from((TICK * ticks).as_nanos()).expect("nanoseconds"))
    }

    fn wheel() -> Wheel {
        Wheel::new(TICK, 8)
    }

    fn fired(wheel: &mut Wheel, at: Moment) -> Vec<u32> {
        let mut expired = Vec::new();
        wheel.advance(at, &mut expired);
        expired.sort_unstable();
        expired
    }

    #[test]
    fn a_timer_fires_once_its_deadline_passed_and_not_before() {
        let mut wheel = wheel();
        wheel.schedule(1, at(5));
        assert_eq!(fired(&mut wheel, at(4)), Vec::<u32>::new());
        assert_eq!(fired(&mut wheel, at(5)), [1]);
        assert_eq!(fired(&mut wheel, at(50)), Vec::<u32>::new());
    }

    #[test]
    fn a_cancelled_timer_never_fires_and_a_moved_one_fires_at_its_new_deadline() {
        let mut wheel = wheel();
        wheel.schedule(1, at(3));
        wheel.schedule(2, at(3));
        wheel.cancel(1);
        wheel.schedule(2, at(9));
        assert_eq!(fired(&mut wheel, at(8)), Vec::<u32>::new());
        assert_eq!(fired(&mut wheel, at(9)), [2]);
    }

    #[test]
    fn timers_of_one_bucket_fire_together_and_a_long_jump_fires_everything_due() {
        let mut wheel = wheel();
        for id in 0..5 {
            wheel.schedule(id, at(7));
        }
        wheel.schedule(5, at(20));
        wheel.schedule(6, at(10_000));
        assert_eq!(fired(&mut wheel, at(30)), [0, 1, 2, 3, 4, 5]);
        assert!(wheel.armed_due(6).is_some());
    }

    #[test]
    fn a_deadline_beyond_the_span_waits_in_the_wheel_until_it_is_rescheduled() {
        let mut wheel = wheel();
        wheel.schedule(3, at(9_000));
        let due = wheel.armed_due(3).expect("armed");
        assert!(due < BUCKETS as u64);
        assert!(wheel.next_wake(at(0)).is_some());
    }

    #[test]
    fn the_wake_is_the_earliest_deadline_and_none_when_the_wheel_is_empty() {
        let mut wheel = wheel();
        assert_eq!(wheel.next_wake(at(0)), None);
        wheel.schedule(1, at(40));
        wheel.schedule(2, at(12));
        assert_eq!(wheel.next_wake(at(0)), Some(TICK * 12));
        wheel.cancel(2);
        assert_eq!(wheel.next_wake(at(0)), Some(TICK * 40));
        wheel.cancel(1);
        assert_eq!(wheel.next_wake(at(0)), None);
    }

    #[test]
    fn the_wake_wraps_around_the_end_of_the_wheel() {
        let mut wheel = wheel();
        assert_eq!(fired(&mut wheel, at(4_090)), Vec::<u32>::new());
        wheel.schedule(1, at(4_100));
        wheel.schedule(2, at(4_093));
        assert_eq!(wheel.next_wake(at(4_090)), Some(TICK * 3));
        assert_eq!(fired(&mut wheel, at(4_093)), [2]);
        assert_eq!(wheel.next_wake(at(4_093)), Some(TICK * 7));
        assert_eq!(fired(&mut wheel, at(4_100)), [1]);
    }

    #[test]
    fn ensure_keeps_a_timer_due_before_the_deadline_and_moves_a_later_one() {
        let mut wheel = wheel();
        wheel.ensure(1, at(5));
        assert_eq!(wheel.armed_due(1), Some(5));
        wheel.ensure(1, at(9));
        assert_eq!(wheel.armed_due(1), Some(5));
        wheel.ensure(1, at(2));
        assert_eq!(wheel.armed_due(1), Some(2));
    }

    #[test]
    fn a_timer_fires_after_a_deadline_exactly_when_its_tick_is_past_the_deadline_tick() {
        let wheel = wheel();
        let tick = wheel.tick_ns;
        for due in 0..40 {
            for nanos in (0..40 * tick).step_by((tick / 3) as usize) {
                let deadline = Moment::from_nanos(nanos);
                assert_eq!(
                    wheel.fires_after(due, deadline),
                    due > wheel.ceil_tick_of(deadline),
                    "due {due} deadline {nanos}"
                );
            }
        }
    }
}
