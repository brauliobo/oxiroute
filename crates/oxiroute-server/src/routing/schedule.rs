//! Which waiting request a saturated pool serves next: class by weight or strict priority, tenants
//! of a class by equal turns, arrival order within a tenant.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use http::{HeaderMap, HeaderName};
use oxiroute_config::{PoolQueuePolicy, QueueDiscipline};

use super::PoolWaiter;

/// The tenant that shares a class with the others when no header names one or the table is full.
const OTHER_TENANT: u32 = 0;

/// Where a request queues: its class and tenant, as resolved by the pool schedule.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct QueueTag {
    class: usize,
    tenant: u32,
}

/// The queue policy of one pool, shared by its tags and its wait queue.
#[derive(Debug)]
pub(super) struct QueueSchedule {
    classes: Box<[(String, u16)]>,
    strict: bool,
    aging: Option<Duration>,
    class_header: Option<HeaderName>,
    tenant_header: Option<HeaderName>,
    default_class: usize,
    max_tenants: usize,
    tenants: Mutex<HashMap<String, u32>>,
}

impl QueueSchedule {
    pub(super) fn from_config(policy: &PoolQueuePolicy) -> Self {
        let classes: Box<[(String, u16)]> = if policy.classes.is_empty() {
            Box::new([("default".to_owned(), 1)])
        } else {
            policy
                .classes
                .iter()
                .map(|class| (class.name.clone(), class.weight))
                .collect()
        };
        let default_class = policy
            .default_class
            .as_ref()
            .and_then(|name| classes.iter().position(|(class, _)| class == name))
            .unwrap_or(classes.len() - 1);
        let header = |name: &Option<String>| {
            name.as_ref().map(|name| {
                HeaderName::from_bytes(name.as_bytes()).expect("validated queue header")
            })
        };
        Self {
            classes,
            strict: policy.discipline == QueueDiscipline::Strict,
            aging: policy.aging_ms.map(Duration::from_millis),
            class_header: header(&policy.class_header),
            tenant_header: header(&policy.tenant_header),
            default_class,
            max_tenants: usize::from(policy.max_tenants),
            tenants: Mutex::new(HashMap::new()),
        }
    }

    pub(super) fn class_names(&self) -> impl Iterator<Item = &str> {
        self.classes.iter().map(|(name, _)| name.as_str())
    }

    /// The route's class wins over the class header; unknown names fall to the default class.
    pub(super) fn tag(&self, route_class: Option<&str>, headers: &HeaderMap) -> QueueTag {
        let named = route_class.or_else(|| {
            self.class_header
                .as_ref()
                .and_then(|name| headers.get(name))
                .and_then(|value| value.to_str().ok())
        });
        let class = named
            .and_then(|name| {
                self.classes
                    .iter()
                    .position(|(class, _)| class == name.trim())
            })
            .unwrap_or(self.default_class);
        let tenant = self
            .tenant_header
            .as_ref()
            .and_then(|name| headers.get(name))
            .and_then(|value| value.to_str().ok())
            .map_or(OTHER_TENANT, |name| self.tenant(name.trim()));
        QueueTag { class, tenant }
    }

    fn tenant(&self, name: &str) -> u32 {
        let mut tenants = self
            .tenants
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(tenant) = tenants.get(name) {
            return *tenant;
        }
        if tenants.len() >= self.max_tenants {
            return OTHER_TENANT;
        }
        let tenant = u32::try_from(tenants.len() + 1).unwrap_or(OTHER_TENANT);
        tenants.insert(name.to_owned(), tenant);
        tenant
    }
}

#[derive(Debug)]
struct Entry {
    waiter: Arc<PoolWaiter>,
    tag: QueueTag,
    seq: u64,
    since: Instant,
}

/// The waiting requests of a pool in service order.
#[derive(Debug)]
pub(super) struct WaitQueue {
    schedule: Option<Arc<QueueSchedule>>,
    entries: Vec<Entry>,
    next_seq: u64,
    /// Virtual time of each class: served requests divided by its weight.
    class_time: Vec<f64>,
    /// Requests served per class and tenant.
    tenant_served: HashMap<(usize, u32), u64>,
    /// The class virtual time of the latest service.
    virtual_time: f64,
    /// Per class, the served count of the tenant most recently served.
    tenant_time: HashMap<usize, u64>,
}

impl WaitQueue {
    pub(super) fn new(schedule: Option<Arc<QueueSchedule>>) -> Self {
        let classes = schedule
            .as_ref()
            .map_or(1, |schedule| schedule.classes.len());
        Self {
            schedule,
            entries: Vec::new(),
            next_seq: 0,
            class_time: vec![0.0; classes],
            tenant_served: HashMap::new(),
            virtual_time: 0.0,
            tenant_time: HashMap::new(),
        }
    }

    pub(super) fn push(&mut self, waiter: Arc<PoolWaiter>, tag: QueueTag) {
        self.catch_up(tag);
        self.next_seq += 1;
        self.entries.push(Entry {
            waiter,
            tag,
            seq: self.next_seq,
            since: Instant::now(),
        });
    }

    /// A class or tenant that was idle starts at the current virtual time, so idleness earns no credit.
    fn catch_up(&mut self, tag: QueueTag) {
        let waiting = |entry: &&Entry| entry.tag.class == tag.class;
        if !self
            .entries
            .iter()
            .any(|entry| entry.tag.class == tag.class)
        {
            let time = &mut self.class_time[tag.class];
            *time = time.max(self.virtual_time);
        }
        if !self
            .entries
            .iter()
            .filter(waiting)
            .any(|entry| entry.tag.tenant == tag.tenant)
        {
            let now = self.tenant_time.get(&tag.class).copied().unwrap_or(0);
            let served = self
                .tenant_served
                .entry((tag.class, tag.tenant))
                .or_insert(0);
            *served = (*served).max(now);
        }
    }

    /// The waiter that may select next.
    pub(super) fn head(&self) -> Option<&Arc<PoolWaiter>> {
        let Some(schedule) = &self.schedule else {
            return self.entries.first().map(|entry| &entry.waiter);
        };
        if let Some(aging) = schedule.aging.filter(|_| schedule.strict) {
            let aged = self
                .entries
                .iter()
                .filter(|entry| entry.since.elapsed() >= aging)
                .min_by_key(|entry| entry.seq);
            if let Some(entry) = aged {
                return Some(&entry.waiter);
            }
        }
        let class = self.next_class(schedule)?;
        self.entries
            .iter()
            .filter(|entry| entry.tag.class == class)
            .min_by_key(|entry| {
                let served = self
                    .tenant_served
                    .get(&(class, entry.tag.tenant))
                    .copied()
                    .unwrap_or(0);
                (served, entry.seq)
            })
            .map(|entry| &entry.waiter)
    }

    fn next_class(&self, schedule: &QueueSchedule) -> Option<usize> {
        let waiting = self.entries.iter().map(|entry| entry.tag.class);
        if schedule.strict {
            return waiting.min();
        }
        waiting.min_by(|a, b| {
            self.class_time[*a]
                .total_cmp(&self.class_time[*b])
                .then(a.cmp(b))
        })
    }

    /// Removes a waiter and reports whether it was the head. A served waiter spends its turn.
    pub(super) fn remove(&mut self, waiter: &Arc<PoolWaiter>, served: bool) -> bool {
        let was_head = self.head().is_some_and(|head| Arc::ptr_eq(head, waiter));
        let Some(index) = self
            .entries
            .iter()
            .position(|entry| Arc::ptr_eq(&entry.waiter, waiter))
        else {
            return false;
        };
        let entry = self.entries.remove(index);
        if served && let Some(schedule) = &self.schedule {
            let weight = f64::from(schedule.classes[entry.tag.class].1);
            let time = &mut self.class_time[entry.tag.class];
            self.virtual_time = self.virtual_time.max(*time);
            *time += 1.0 / weight;
            let served = self
                .tenant_served
                .entry((entry.tag.class, entry.tag.tenant))
                .or_insert(0);
            let now = self.tenant_time.entry(entry.tag.class).or_insert(0);
            *now = (*now).max(*served);
            *served += 1;
        }
        was_head
    }

    pub(super) fn is_head(&self, waiter: &Arc<PoolWaiter>) -> bool {
        self.head().is_some_and(|head| Arc::ptr_eq(head, waiter))
    }

    /// Waiting requests per class, in class order.
    pub(super) fn depth_by_class(&self) -> Vec<u64> {
        let mut depth = vec![0; self.class_time.len()];
        for entry in &self.entries {
            depth[entry.tag.class] += 1;
        }
        depth
    }
}

#[cfg(test)]
mod tests {
    use oxiroute_config::PoolQueueClass;
    use tokio::sync::Notify;

    use super::*;

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
        let mut labels = Vec::new();
        for (class, tenant, count) in waiting {
            for _ in 0..*count {
                let waiter = waiter();
                queue.push(
                    Arc::clone(&waiter),
                    schedule.tag(None, &headers(class, tenant)),
                );
                labels.push((waiter, (*class).to_owned(), (*tenant).to_owned()));
            }
        }
        let mut order = Vec::new();
        while let Some(head) = queue.head().cloned() {
            let (_, class, tenant) = labels
                .iter()
                .find(|(waiter, _, _)| Arc::ptr_eq(waiter, &head))
                .expect("known waiter");
            order.push((class.clone(), tenant.clone()));
            assert!(queue.remove(&head, true));
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
        let old = waiter();
        queue.push(Arc::clone(&old), schedule.tag(None, &headers("batch", "a")));
        std::thread::sleep(Duration::from_millis(150));
        queue.push(waiter(), schedule.tag(None, &headers("interactive", "a")));

        assert!(queue.is_head(&old));
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
            let served = waiter();
            queue.push(
                Arc::clone(&served),
                schedule.tag(None, &headers("batch", "a")),
            );
            queue.remove(&served, true);
        }
        let mut waiting = Vec::new();
        for class in ["interactive", "batch"] {
            for _ in 0..8 {
                let waiter = waiter();
                queue.push(
                    Arc::clone(&waiter),
                    schedule.tag(None, &headers(class, "a")),
                );
                waiting.push((waiter, class));
            }
        }

        let mut first = Vec::new();
        for _ in 0..5 {
            let head = queue.head().cloned().expect("head");
            first.push(
                waiting
                    .iter()
                    .find(|(w, _)| Arc::ptr_eq(w, &head))
                    .expect("known")
                    .1,
            );
            queue.remove(&head, true);
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
        let (first, second) = (waiter(), waiter());
        queue.push(Arc::clone(&first), QueueTag::default());
        queue.push(Arc::clone(&second), QueueTag::default());

        assert!(queue.is_head(&first));
        assert!(queue.remove(&first, true));
        assert!(queue.is_head(&second));
    }
}
