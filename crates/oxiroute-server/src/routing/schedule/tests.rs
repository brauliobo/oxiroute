use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::Duration,
};

use http::HeaderMap;
use oxiroute_config::{PoolQueueClass, PoolQueuePolicy, QueueDiscipline};
use tokio::sync::Notify;

use super::*;
use crate::routing::PoolWaiter;

fn policy(discipline: QueueDiscipline, aging_ms: Option<u64>) -> PoolQueuePolicy {
    PoolQueuePolicy {
        classes: vec![
            PoolQueueClass {
                name: "interactive".into(),
                weight: 3,
            },
            PoolQueueClass {
                name: "batch".into(),
                weight: 1,
            },
        ],
        discipline,
        class_header: Some("x-priority".into()),
        default_class: Some("batch".into()),
        tenant_header: Some("x-tenant".into()),
        max_tenants: 2,
        aging_ms,
    }
}

fn waiter() -> Arc<PoolWaiter> {
    Arc::new(PoolWaiter {
        notify: Notify::new(),
    })
}

fn headers(class: &str, tenant: &str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert("x-priority", class.parse().expect("class"));
    headers.insert("x-tenant", tenant.parse().expect("tenant"));
    headers
}

/// Queues `count` waiters per (class, tenant) and serves everything, returning the order served.
fn serve_all(
    schedule: &Arc<QueueSchedule>,
    waiting: &[(&str, &str, usize)],
) -> Vec<(String, String)> {
    let mut queue = WaitQueue::new(Some(Arc::clone(schedule)));
    let mut labels = HashMap::new();
    for (class, tenant, count) in waiting {
        for _ in 0..*count {
            let seq = queue.push(waiter(), schedule.tag(None, &headers(class, tenant)));
            labels.insert(seq, ((*class).to_owned(), (*tenant).to_owned()));
        }
    }
    let mut order = Vec::new();
    while let Some((head, _)) = queue.head() {
        order.push(labels[&head].clone());
        assert!(queue.remove(head, true));
    }
    order
}

fn count(order: &[(String, String)], class: &str, tenant: Option<&str>) -> usize {
    order
        .iter()
        .filter(|(c, t)| c == class && tenant.is_none_or(|tenant| t == tenant))
        .count()
}

#[test]
fn weighted_classes_share_service_by_weight_while_both_wait() {
    let schedule = Arc::new(QueueSchedule::from_config(&policy(
        QueueDiscipline::Weighted,
        None,
    )));

    let order = serve_all(&schedule, &[("interactive", "a", 12), ("batch", "a", 12)]);

    assert_eq!(count(&order[..8], "interactive", None), 6, "{order:?}");
    assert_eq!(count(&order[..8], "batch", None), 2);
}

#[test]
fn strict_classes_serve_the_higher_class_first() {
    let schedule = Arc::new(QueueSchedule::from_config(&policy(
        QueueDiscipline::Strict,
        None,
    )));

    let order = serve_all(&schedule, &[("batch", "a", 3), ("interactive", "a", 3)]);

    assert!(
        order[..3].iter().all(|(class, _)| class == "interactive"),
        "{order:?}"
    );
}

#[test]
fn aging_lets_an_old_low_class_request_overtake() {
    let schedule = Arc::new(QueueSchedule::from_config(&policy(
        QueueDiscipline::Strict,
        Some(100),
    )));
    let mut queue = WaitQueue::new(Some(Arc::clone(&schedule)));
    let old = queue.push(waiter(), schedule.tag(None, &headers("batch", "a")));
    std::thread::sleep(Duration::from_millis(150));
    queue.push(waiter(), schedule.tag(None, &headers("interactive", "a")));

    assert!(queue.is_head(old));
}

#[test]
fn tenants_of_a_class_take_turns_so_a_flood_does_not_delay_a_small_tenant() {
    let schedule = Arc::new(QueueSchedule::from_config(&policy(
        QueueDiscipline::Weighted,
        None,
    )));

    let order = serve_all(&schedule, &[("batch", "flood", 50), ("batch", "small", 5)]);

    assert_eq!(count(&order[..10], "batch", Some("small")), 5, "{order:?}");
}

#[test]
fn a_class_that_was_idle_earns_no_credit() {
    let schedule = Arc::new(QueueSchedule::from_config(&policy(
        QueueDiscipline::Weighted,
        None,
    )));
    let mut queue = WaitQueue::new(Some(Arc::clone(&schedule)));
    for _ in 0..10 {
        let served = queue.push(waiter(), schedule.tag(None, &headers("batch", "a")));
        queue.remove(served, true);
    }
    let mut waiting = HashMap::new();
    for class in ["interactive", "batch"] {
        for _ in 0..8 {
            let seq = queue.push(waiter(), schedule.tag(None, &headers(class, "a")));
            waiting.insert(seq, class);
        }
    }

    let mut first = Vec::new();
    for _ in 0..5 {
        let (head, _) = queue.head().expect("head");
        first.push(waiting[&head]);
        queue.remove(head, true);
    }

    assert!(
        first.contains(&"batch"),
        "interactive must not burst: {first:?}"
    );
}

#[test]
fn unknown_classes_and_surplus_tenants_fall_back() {
    let schedule = QueueSchedule::from_config(&policy(QueueDiscipline::Weighted, None));
    let tag = |class, tenant| schedule.tag(None, &headers(class, tenant));

    assert_eq!(tag("nonsense", "a").class, 1, "default class is batch");
    assert_eq!(tag("interactive", "a").class, 0);
    assert_eq!(
        schedule
            .tag(Some("interactive"), &headers("batch", "a"))
            .class,
        0,
        "route wins"
    );
    let (a, b, c) = (tag("batch", "a"), tag("batch", "b"), tag("batch", "c"));
    assert!(
        a.tenant != b.tenant && c.tenant == OTHER_TENANT,
        "{a:?} {b:?} {c:?}"
    );
}

#[test]
fn without_a_schedule_waiters_are_served_in_arrival_order() {
    let mut queue = WaitQueue::new(None);
    let first = queue.push(waiter(), QueueTag::default());
    let second = queue.push(waiter(), QueueTag::default());

    assert!(queue.is_head(first));
    assert!(queue.remove(first, true));
    assert!(queue.is_head(second));
}

#[test]
fn removing_a_waiter_from_the_middle_keeps_the_arrival_order_of_the_rest() {
    let mut queue = WaitQueue::new(None);
    let seqs = (0..5)
        .map(|_| queue.push(waiter(), QueueTag::default()))
        .collect::<Vec<_>>();

    assert!(!queue.remove(seqs[2], false));
    assert!(!queue.remove(seqs[2], false), "a second removal is a no-op");
    let order = std::iter::from_fn(|| {
        let (head, _) = queue.head()?;
        queue.remove(head, true);
        Some(head)
    })
    .collect::<Vec<_>>();

    assert_eq!(order, [seqs[0], seqs[1], seqs[3], seqs[4]]);
    assert_eq!(queue.len(), 0);
}

#[test]
fn depth_by_class_counts_the_waiting_requests_of_each_class() {
    let schedule = Arc::new(QueueSchedule::from_config(&policy(
        QueueDiscipline::Weighted,
        None,
    )));
    let mut queue = WaitQueue::new(Some(Arc::clone(&schedule)));
    for class in ["interactive", "batch", "batch"] {
        queue.push(waiter(), schedule.tag(None, &headers(class, "a")));
    }

    assert_eq!(queue.depth_by_class(), [1, 2]);
}

/// The scan the queue used before it was indexed: the reference every operation must agree with.
struct Scanned {
    schedule: Arc<QueueSchedule>,
    entries: Vec<(u64, QueueTag)>,
    class_time: Vec<f64>,
    tenant_served: HashMap<(usize, u32), u64>,
    virtual_time: f64,
    tenant_time: HashMap<usize, u64>,
}

impl Scanned {
    fn new(schedule: &Arc<QueueSchedule>) -> Self {
        Self {
            schedule: Arc::clone(schedule),
            entries: Vec::new(),
            class_time: vec![0.0; schedule.classes.len()],
            tenant_served: HashMap::new(),
            virtual_time: 0.0,
            tenant_time: HashMap::new(),
        }
    }

    fn push(&mut self, seq: u64, tag: QueueTag) {
        if !self
            .entries
            .iter()
            .any(|(_, entry)| entry.class == tag.class)
        {
            let time = &mut self.class_time[tag.class];
            *time = time.max(self.virtual_time);
        }
        if !self.entries.iter().any(|(_, entry)| *entry == tag) {
            let now = self.tenant_time.get(&tag.class).copied().unwrap_or(0);
            let served = self
                .tenant_served
                .entry((tag.class, tag.tenant))
                .or_insert(0);
            *served = (*served).max(now);
        }
        self.entries.push((seq, tag));
    }

    fn head(&self) -> Option<u64> {
        let class = if self.schedule.strict {
            self.entries.iter().map(|(_, tag)| tag.class).min()
        } else {
            self.entries
                .iter()
                .map(|(_, tag)| tag.class)
                .min_by(|a, b| {
                    self.class_time[*a]
                        .total_cmp(&self.class_time[*b])
                        .then(a.cmp(b))
                })
        }?;
        self.entries
            .iter()
            .filter(|(_, tag)| tag.class == class)
            .min_by_key(|(seq, tag)| {
                (
                    self.tenant_served
                        .get(&(class, tag.tenant))
                        .copied()
                        .unwrap_or(0),
                    *seq,
                )
            })
            .map(|(seq, _)| *seq)
    }

    fn remove(&mut self, seq: u64, served: bool) {
        let index = self
            .entries
            .iter()
            .position(|(entry, _)| *entry == seq)
            .expect("queued");
        let (_, tag) = self.entries.remove(index);
        if served {
            let time = &mut self.class_time[tag.class];
            self.virtual_time = self.virtual_time.max(*time);
            *time += 1.0 / f64::from(self.schedule.classes[tag.class].1);
            let served = self
                .tenant_served
                .entry((tag.class, tag.tenant))
                .or_insert(0);
            let now = self.tenant_time.entry(tag.class).or_insert(0);
            *now = (*now).max(*served);
            *served += 1;
        }
    }
}

fn pick(state: &mut u64, bound: usize) -> usize {
    let bound = u64::try_from(bound).expect("bound");
    usize::try_from(pseudo_random(state) % bound).expect("index")
}

fn pseudo_random(state: &mut u64) -> u64 {
    *state ^= *state << 13;
    *state ^= *state >> 7;
    *state ^= *state << 17;
    *state
}

#[test]
fn the_indexed_queue_serves_in_the_order_of_the_scan_it_replaced() {
    for discipline in [QueueDiscipline::Weighted, QueueDiscipline::Strict] {
        let schedule = Arc::new(QueueSchedule::from_config(&policy(discipline, None)));
        let mut queue = WaitQueue::new(Some(Arc::clone(&schedule)));
        let mut reference = Scanned::new(&schedule);
        let mut waiting = Vec::new();
        let mut state = 0x9E37_79B9_7F4A_7C15_u64;
        for step in 0..4000 {
            if waiting.is_empty() || pseudo_random(&mut state) % 5 < 3 {
                let class = ["interactive", "batch"][pick(&mut state, 2)];
                let tenant = ["a", "b", "c"][pick(&mut state, 3)];
                let tag = schedule.tag(None, &headers(class, tenant));
                let seq = queue.push(waiter(), tag);
                reference.push(seq, tag);
                waiting.push(seq);
            } else {
                let seq = if pseudo_random(&mut state).is_multiple_of(2) {
                    reference.head().expect("head")
                } else {
                    waiting[pick(&mut state, waiting.len())]
                };
                let served = !pseudo_random(&mut state).is_multiple_of(4);
                let was_head = reference.head() == Some(seq);
                assert_eq!(queue.remove(seq, served), was_head, "step {step}");
                reference.remove(seq, served);
                waiting.retain(|waiting| *waiting != seq);
            }
            assert_eq!(
                queue.head().map(|(seq, _)| seq),
                reference.head(),
                "step {step}"
            );
        }
        let distinct = waiting.iter().collect::<HashSet<_>>();
        assert_eq!(queue.len(), distinct.len());
    }
}
