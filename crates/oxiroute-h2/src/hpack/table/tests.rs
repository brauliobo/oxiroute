use super::{ENTRY_OVERHEAD, Found, Table};
use std::collections::VecDeque;

struct Model {
    entries: VecDeque<(Vec<u8>, Vec<u8>)>,
    max: usize,
}

impl Model {
    fn size(&self) -> usize {
        self.entries
            .iter()
            .map(|(n, v)| n.len() + v.len() + ENTRY_OVERHEAD)
            .sum()
    }

    fn evict_to(&mut self, limit: usize) {
        while self.size() > limit {
            self.entries.pop_front();
        }
    }

    fn insert(&mut self, name: &[u8], value: &[u8]) {
        let need = name.len() + value.len() + ENTRY_OVERHEAD;
        self.evict_to(self.max.saturating_sub(need));
        if need <= self.max {
            self.entries.push_back((name.to_vec(), value.to_vec()));
        }
    }

    fn find(&self, name: &[u8], value: &[u8]) -> Found {
        let mut found = Found::default();
        for (n, (en, ev)) in self.entries.iter().rev().enumerate() {
            if en == name {
                found.name.get_or_insert(n + 1);
                if ev == value {
                    found.full = Some(n + 1);
                    break;
                }
            }
        }
        found
    }
}

struct Rng(u64);

impl Rng {
    fn below(&mut self, n: usize) -> usize {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        usize::try_from(self.0 % n as u64).unwrap()
    }

    fn bytes(&mut self, max: usize) -> Vec<u8> {
        let len = self.below(max);
        (0..len)
            .map(|_| b'a' + u8::try_from(self.below(3)).unwrap())
            .collect()
    }
}

fn assert_same(table: &Table, model: &Model) {
    assert_eq!(table.size(), model.size());
    for n in 1..=model.entries.len() {
        let (name, value) = &model.entries[model.entries.len() - n];
        assert_eq!(
            table.get(n),
            Some((name.as_slice(), value.as_slice())),
            "entry {n}"
        );
    }
    assert_eq!(table.get(0), None);
    assert_eq!(table.get(model.entries.len() + 1), None);
}

#[test]
fn follows_a_model_through_wraps_evictions_and_reclaims() {
    let mut rng = Rng(0x2545_f491_4f6c_dd1d);
    for max in [0, 31, 32, 64, 200, 1000, 4096, 20_000] {
        let mut table = Table::new(max);
        let mut model = Model {
            entries: VecDeque::new(),
            max,
        };
        for step in 0..4000 {
            match rng.below(40) {
                0 => {
                    let new_max = *[0, 32, 100, 500, 4096].get(rng.below(5)).unwrap();
                    table.set_max(new_max);
                    model.max = new_max;
                    model.evict_to(new_max);
                }
                1 => {
                    table.clear();
                    model.evict_to(0);
                }
                _ => {
                    let long = rng.below(10) == 0;
                    let (name, value) = (rng.bytes(6), rng.bytes(if long { 700 } else { 40 }));
                    assert_eq!(
                        table.find(&name, &value),
                        model.find(&name, &value),
                        "max {max} step {step}"
                    );
                    table.insert(&name, &value);
                    model.insert(&name, &value);
                }
            }
            assert_same(&table, &model);
        }
    }
}

#[test]
fn arena_stays_bounded_by_the_live_bytes() {
    let mut table = Table::new(4096);
    for i in 0..100_000u32 {
        table.insert(b"name", &i.to_be_bytes().repeat(8));
    }
    assert!(table.arena.len() <= 2 * 4096 + 64, "{}", table.arena.len());
    assert_eq!(table.len(), 4096 / (4 + 32 + 32));
}

#[test]
fn offsets_survive_wrapping_past_u32() {
    let mut table = Table::new(300);
    table.base = u32::MAX - 100;
    for i in 0..50u8 {
        table.insert(b"k", &[i; 20]);
        assert_eq!(table.get(1), Some((b"k".as_slice(), [i; 20].as_slice())));
    }
}
