use super::{client::Pooled, config::Config};

const SPARE_HEADROOM: usize = 16;

#[derive(Default)]
pub(crate) struct BufferPool {
    spare: Vec<Pooled>,
    leased: usize,
}

impl BufferPool {
    pub(crate) fn lease(&mut self, config: &Config) -> Pooled {
        self.leased += 1;
        self.spare.pop().unwrap_or_else(|| Pooled::new(config))
    }

    pub(crate) fn give_back(&mut self, buffers: Pooled) {
        self.leased -= 1;
        let kept = SPARE_HEADROOM + self.leased;
        self.spare.truncate(kept);
        if self.spare.len() < kept {
            self.spare.push(buffers);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lease_all(pool: &mut BufferPool, count: usize) -> Vec<Pooled> {
        let config = Config::default();
        (0..count).map(|_| pool.lease(&config)).collect()
    }

    #[test]
    fn a_burst_of_leases_leaves_only_the_headroom_once_it_has_ended() {
        let mut pool = BufferPool::default();
        for buffers in lease_all(&mut pool, 200) {
            pool.give_back(buffers);
        }
        assert_eq!(pool.spare.len(), SPARE_HEADROOM);
        assert_eq!(pool.leased, 0);
    }

    #[test]
    fn buffers_are_kept_for_as_many_requests_as_are_in_flight() {
        let mut pool = BufferPool::default();
        let mut held = lease_all(&mut pool, 200);
        let config = Config::default();
        for _ in 0..1_000 {
            let buffers = held.pop().expect("a lease");
            pool.give_back(buffers);
            held.push(pool.lease(&config));
            assert!(pool.spare.len() <= 1);
        }
        assert_eq!(pool.leased, 200);
    }
}
