use super::{arrival::Arrival, fleet::Plan, order::Order, placement::Placement};

pub(crate) struct Schedule<A> {
    pub(crate) placement: Placement<A>,
    pub(crate) order: Order,
    pub(crate) arrivals: Vec<Arrival<A>>,
    pub(crate) shared: Vec<bool>,
}

impl<A> Schedule<A> {
    pub(crate) fn new(plan: Plan<A>, shared: Vec<bool>) -> Self {
        Self {
            placement: plan.placement,
            order: Order::new(plan.age),
            arrivals: Vec::new(),
            shared,
        }
    }

    pub(crate) fn serves(&self, listener: usize) -> bool {
        self.placement.active() || !self.shared[listener]
    }
}
