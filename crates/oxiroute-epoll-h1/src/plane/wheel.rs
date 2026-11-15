use std::time::{Duration, Instant};

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
    base: Instant,
    tick_ns: u64,
    cursor: u64,
    heads: Box<[u32; BUCKETS]>,
    occupied: [u64; WORDS],
    nodes: Vec<Node>,
}

impl Wheel {
    pub(crate) fn new(base: Instant, tick: Duration, capacity: usize) -> Self {
        Self {
            base,
            tick_ns: u64::try_from(tick.as_nanos()).unwrap_or(u64::MAX).max(1),
            cursor: 0,
            heads: Box::new([NONE; BUCKETS]),
            occupied: [0; WORDS],
            nodes: vec![UNARMED; capacity],
        }
    }

    pub(crate) fn tick_of(&self, at: Instant) -> u64 {
        let elapsed = at.saturating_duration_since(self.base).as_nanos();
        u64::try_from(elapsed / u128::from(self.tick_ns)).unwrap_or(u64::MAX)
    }

    fn ceil_tick_of(&self, at: Instant) -> u64 {
        let elapsed = at.saturating_duration_since(self.base).as_nanos();
        let tick = u128::from(self.tick_ns);
        u64::try_from(elapsed.div_ceil(tick)).unwrap_or(u64::MAX)
    }

    pub(crate) fn armed_due(&self, id: u32) -> Option<u64> {
        let node = self.nodes[id as usize];
        node.armed.then_some(node.due)
    }

    pub(crate) fn ensure(&mut self, id: u32, deadline: Instant) {
        if self
            .armed_due(id)
            .is_none_or(|due| due > self.ceil_tick_of(deadline))
        {
            self.schedule(id, deadline);
        }
    }

    pub(crate) fn schedule(&mut self, id: u32, deadline: Instant) {
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

    pub(crate) fn advance(&mut self, now: Instant, expired: &mut Vec<u32>) {
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

    pub(crate) fn next_wake(&self, now: Instant) -> Option<Duration> {
        let start = (self.cursor & MASK) as usize;
        let distance = self.first_occupied_from(start)?;
        let due = self.cursor + distance as u64;
        let wake = self.base + Duration::from_nanos(due.saturating_mul(self.tick_ns));
        Some(wake.saturating_duration_since(now))
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

    const TICK: Duration = Duration::from_millis(10);

    fn wheel() -> (Wheel, Instant) {
        let base = Instant::now();
        (Wheel::new(base, TICK, 8), base)
    }

    fn fired(wheel: &mut Wheel, at: Instant) -> Vec<u32> {
        let mut expired = Vec::new();
        wheel.advance(at, &mut expired);
        expired.sort_unstable();
        expired
    }

    #[test]
    fn a_timer_fires_once_its_deadline_passed_and_not_before() {
        let (mut wheel, base) = wheel();
        wheel.schedule(1, base + TICK * 5);
        assert_eq!(fired(&mut wheel, base + TICK * 4), Vec::<u32>::new());
        assert_eq!(fired(&mut wheel, base + TICK * 5), [1]);
        assert_eq!(fired(&mut wheel, base + TICK * 50), Vec::<u32>::new());
    }

    #[test]
    fn a_cancelled_timer_never_fires_and_a_moved_one_fires_at_its_new_deadline() {
        let (mut wheel, base) = wheel();
        wheel.schedule(1, base + TICK * 3);
        wheel.schedule(2, base + TICK * 3);
        wheel.cancel(1);
        wheel.schedule(2, base + TICK * 9);
        assert_eq!(fired(&mut wheel, base + TICK * 8), Vec::<u32>::new());
        assert_eq!(fired(&mut wheel, base + TICK * 9), [2]);
    }

    #[test]
    fn timers_of_one_bucket_fire_together_and_a_long_jump_fires_everything_due() {
        let (mut wheel, base) = wheel();
        for id in 0..5 {
            wheel.schedule(id, base + TICK * 7);
        }
        wheel.schedule(5, base + TICK * 20);
        wheel.schedule(6, base + TICK * 10_000);
        assert_eq!(fired(&mut wheel, base + TICK * 30), [0, 1, 2, 3, 4, 5]);
        assert!(wheel.armed_due(6).is_some());
    }

    #[test]
    fn a_deadline_beyond_the_span_waits_in_the_wheel_until_it_is_rescheduled() {
        let (mut wheel, base) = wheel();
        wheel.schedule(3, base + TICK * 9_000);
        let due = wheel.armed_due(3).expect("armed");
        assert!(due < BUCKETS as u64);
        assert!(wheel.next_wake(base).is_some());
    }

    #[test]
    fn the_wake_is_the_earliest_deadline_and_none_when_the_wheel_is_empty() {
        let (mut wheel, base) = wheel();
        assert_eq!(wheel.next_wake(base), None);
        wheel.schedule(1, base + TICK * 40);
        wheel.schedule(2, base + TICK * 12);
        assert_eq!(wheel.next_wake(base), Some(TICK * 12));
        wheel.cancel(2);
        assert_eq!(wheel.next_wake(base), Some(TICK * 40));
        wheel.cancel(1);
        assert_eq!(wheel.next_wake(base), None);
    }

    #[test]
    fn the_wake_wraps_around_the_end_of_the_wheel() {
        let (mut wheel, base) = wheel();
        assert_eq!(fired(&mut wheel, base + TICK * 4_090), Vec::<u32>::new());
        wheel.schedule(1, base + TICK * 4_100);
        wheel.schedule(2, base + TICK * 4_093);
        assert_eq!(wheel.next_wake(base + TICK * 4_090), Some(TICK * 3));
        assert_eq!(fired(&mut wheel, base + TICK * 4_093), [2]);
        assert_eq!(wheel.next_wake(base + TICK * 4_093), Some(TICK * 7));
        assert_eq!(fired(&mut wheel, base + TICK * 4_100), [1]);
    }
}
