use std::collections::VecDeque;

use super::slab::Token;

pub(crate) struct Starts {
    queue: VecDeque<Token>,
    granted: Option<Token>,
    per_pass: usize,
    budget: usize,
}

impl Starts {
    pub(crate) fn new(per_pass: usize) -> Self {
        Self {
            queue: VecDeque::new(),
            granted: None,
            per_pass,
            budget: per_pass,
        }
    }

    pub(crate) fn begin_pass(&mut self) {
        self.budget = self.per_pass;
    }

    pub(crate) fn pending(&self) -> bool {
        !self.queue.is_empty()
    }

    pub(crate) fn admit(&mut self, token: Token, queued: &mut bool) -> bool {
        if *queued {
            return false;
        }
        if self.granted == Some(token) {
            self.granted = None;
            return true;
        }
        if self.queue.is_empty() && self.budget > 0 {
            self.budget -= 1;
            return true;
        }
        *queued = true;
        self.queue.push_back(token);
        false
    }

    pub(crate) fn grant(&mut self) -> Option<Token> {
        if self.budget == 0 {
            return None;
        }
        let token = self.queue.pop_front()?;
        self.budget -= 1;
        self.granted = Some(token);
        Some(token)
    }

    pub(crate) fn settle(&mut self) {
        self.granted = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn token(index: u32) -> Token {
        Token {
            index,
            generation: 0,
        }
    }

    #[test]
    fn starts_within_the_budget_are_admitted_and_the_rest_wait_in_arrival_order() {
        let mut starts = Starts::new(2);
        let mut queued = [false; 4];
        let admitted: Vec<bool> = (0..4)
            .map(|index| starts.admit(token(index as u32), &mut queued[index]))
            .collect();
        assert_eq!(admitted, [true, true, false, false]);
        assert_eq!(queued, [false, false, true, true]);
        assert!(starts.pending());
        assert_eq!(starts.grant(), None);
        starts.begin_pass();
        assert_eq!(starts.grant(), Some(token(2)));
        assert_eq!(starts.grant(), Some(token(3)));
        assert_eq!(starts.grant(), None);
        assert!(!starts.pending());
    }

    #[test]
    fn a_new_start_waits_behind_the_queue_even_when_budget_is_left() {
        let mut starts = Starts::new(2);
        let mut queued = [false; 4];
        for index in 0..3 {
            starts.admit(token(index), &mut queued[index as usize]);
        }
        starts.begin_pass();
        assert!(!starts.admit(token(3), &mut queued[3]));
        assert_eq!(starts.grant(), Some(token(2)));
        assert_eq!(starts.grant(), Some(token(3)));
    }

    #[test]
    fn a_waiting_connection_is_queued_once_and_a_granted_one_starts_without_budget() {
        let mut starts = Starts::new(1);
        let mut first = false;
        let mut second = false;
        assert!(starts.admit(token(0), &mut first));
        assert!(!starts.admit(token(1), &mut second));
        assert!(!starts.admit(token(1), &mut second));
        starts.begin_pass();
        let granted = starts.grant().expect("a grant");
        second = false;
        assert!(starts.admit(granted, &mut second));
        assert!(!starts.pending());
    }

    #[test]
    fn an_unused_grant_is_dropped() {
        let mut starts = Starts::new(1);
        let mut queued = false;
        starts.admit(token(0), &mut false);
        starts.admit(token(1), &mut queued);
        starts.begin_pass();
        starts.grant();
        starts.settle();
        let mut other = false;
        assert!(!starts.admit(token(1), &mut other));
    }
}
