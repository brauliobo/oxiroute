//! Which waiting request a saturated pool serves next: class by weight or strict priority, tenants
//! of a class by equal turns, arrival order within a tenant.

use std::{collections::HashMap, sync::Mutex, time::Duration};

use http::{HeaderMap, HeaderName};
use oxiroute_config::{PoolQueuePolicy, QueueDiscipline};

mod wait_queue;
pub(super) use wait_queue::WaitQueue;

/// The headers of a request that a queue tag reads: the first value of a name, when it is visible ASCII.
pub(crate) trait FirstHeader {
    fn first(&self, name: &HeaderName) -> Option<&str>;
}

impl FirstHeader for HeaderMap {
    fn first(&self, name: &HeaderName) -> Option<&str> {
        self.get(name).and_then(|value| value.to_str().ok())
    }
}

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
    pub(super) fn tag(&self, route_class: Option<&str>, headers: &impl FirstHeader) -> QueueTag {
        let named = route_class.or_else(|| {
            self.class_header
                .as_ref()
                .and_then(|name| headers.first(name))
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
            .and_then(|name| headers.first(name))
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

#[cfg(test)]
mod tests;
