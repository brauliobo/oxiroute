use std::num::NonZeroU64;

use openssl::hash::{MessageDigest, hash};

#[allow(clippy::wildcard_imports)]
use super::*;
use crate::routing::carry::{PassiveCarry, ServerHealthCarry, carried, restored};

impl PoolEndpoint {
    pub(in crate::routing) fn identity(&self) -> u64 {
        let digest = hash(
            MessageDigest::sha256(),
            self.endpoint.to_string().as_bytes(),
        )
        .expect("OpenSSL digest");
        u64::from_be_bytes(digest[..8].try_into().expect("a digest has eight bytes"))
    }

    pub(in crate::routing) fn health_carry(&self) -> ServerHealthCarry {
        let load = |counter: &AtomicU64| carried(counter.load(Ordering::Relaxed));
        let passive = PassiveCarry {
            ejected: self.passive_ejected.load(Ordering::Acquire),
            failure_count: load(&self.passive_failure_count),
            consecutive_failures: load(&self.passive_consecutive_failures),
            ejection_count: load(&self.passive_ejection_count),
            backoff_step: load(&self.passive_backoff_step),
            last_failure: HealthFailure::from_u8(self.passive_last_failure.load(Ordering::Relaxed)),
            ejected_at: load(&self.passive_ejected_at_unix_ms),
            ejected_until: load(&self.passive_ejection_until_unix_ms),
            recovery_count: load(&self.passive_recovery_count),
            recovered_at: load(&self.passive_last_recovery_at_unix_ms),
        };
        ServerHealthCarry {
            server: self.name.clone(),
            identity: self.identity(),
            state: self.state(),
            checked_at: load(&self.last_checked_at_unix_ms),
            transitioned_at: load(&self.last_transition_at_unix_ms),
            successes: load(&self.consecutive_successes),
            failures: load(&self.consecutive_failures),
            last_failure: HealthFailure::from_u8(self.last_failure.load(Ordering::Relaxed)),
            passive: (passive != PassiveCarry::default()).then_some(passive),
            breaker: self.breaker.carry(),
        }
    }

    pub(in crate::routing) fn restore_health(&self, carried: &ServerHealthCarry) {
        let checked = self.state() != EndpointHealthState::Unchecked;
        if checked
            && !matches!(
                carried.state,
                EndpointHealthState::Unchecked | EndpointHealthState::Unknown
            )
        {
            self.state.store(carried.state as u8, Ordering::Release);
        }
        let store = |counter: &AtomicU64, value| counter.store(restored(value), Ordering::Relaxed);
        store(&self.last_checked_at_unix_ms, carried.checked_at);
        store(&self.last_transition_at_unix_ms, carried.transitioned_at);
        store(&self.consecutive_successes, carried.successes);
        store(&self.consecutive_failures, carried.failures);
        self.last_failure.store(
            carried.last_failure.map_or(0, |failure| failure as u8),
            Ordering::Relaxed,
        );
        if let Some(passive) = &carried.passive {
            self.restore_passive(passive);
        }
        if let (Some(breaker), Some(_)) = (&carried.breaker, self.breaker_policy) {
            self.breaker.restore(breaker);
        }
    }

    fn restore_passive(&self, passive: &PassiveCarry) {
        let store = |counter: &AtomicU64, value: Option<NonZeroU64>| {
            counter.store(restored(value), Ordering::Relaxed);
        };
        store(&self.passive_failure_count, passive.failure_count);
        store(
            &self.passive_consecutive_failures,
            passive.consecutive_failures,
        );
        store(&self.passive_ejection_count, passive.ejection_count);
        store(&self.passive_backoff_step, passive.backoff_step);
        store(&self.passive_ejected_at_unix_ms, passive.ejected_at);
        store(&self.passive_last_recovery_at_unix_ms, passive.recovered_at);
        store(&self.passive_recovery_count, passive.recovery_count);
        self.passive_last_failure.store(
            passive.last_failure.map_or(0, |failure| failure as u8),
            Ordering::Relaxed,
        );
        self.passive_ejection_until_unix_ms
            .store(restored(passive.ejected_until), Ordering::Release);
        self.passive_ejected
            .store(passive.ejected, Ordering::Release);
    }
}
