use super::*;

fn key(name: &str) -> RegistrationKey {
    RegistrationKey::new("render", name)
}

#[test]
fn leases_expire_in_deadline_order_and_only_after_their_ttl() {
    let clock = ManualClock::new();
    let mut table = LeaseTable::default();
    table.grant(key("slow"), clock.now(), Duration::from_secs(30));
    table.grant(key("fast"), clock.now(), Duration::from_secs(10));

    assert_eq!(table.take_expired(clock.now()), Vec::new());
    clock.advance(Duration::from_secs(10));
    assert_eq!(table.take_expired(clock.now()), [key("fast")]);
    clock.advance(Duration::from_mins(1));
    assert_eq!(table.take_expired(clock.now()), [key("slow")]);
    assert_eq!(table.remaining(&key("slow"), clock.now()), None);
}

#[test]
fn a_renewal_replaces_the_deadline_without_leaving_a_stale_entry() {
    let clock = ManualClock::new();
    let mut table = LeaseTable::default();
    table.grant(key("gpu"), clock.now(), Duration::from_secs(10));
    clock.advance(Duration::from_secs(8));
    table.grant(key("gpu"), clock.now(), Duration::from_secs(10));
    clock.advance(Duration::from_secs(8));

    assert_eq!(table.take_expired(clock.now()), Vec::new());
    assert_eq!(
        table.remaining(&key("gpu"), clock.now()),
        Some(Duration::from_secs(2))
    );
    assert_eq!(
        table.next_deadline(),
        Some(clock.now() + Duration::from_secs(2))
    );
}

#[test]
fn removal_and_unknown_keys_are_inert() {
    let clock = ManualClock::new();
    let mut table = LeaseTable::default();
    table.grant(key("gpu"), clock.now(), Duration::from_secs(1));
    table.remove(&key("gpu"));
    table.remove(&key("other"));

    assert_eq!(table.remaining(&key("gpu"), clock.now()), None);
    assert_eq!(table.next_deadline(), None);
}

#[test]
fn an_overdue_lease_reports_zero_remaining_until_it_is_taken() {
    let clock = ManualClock::new();
    let mut table = LeaseTable::default();
    table.grant(key("gpu"), clock.now(), Duration::from_secs(1));
    clock.advance(Duration::from_secs(5));

    assert_eq!(
        table.remaining(&key("gpu"), clock.now()),
        Some(Duration::ZERO)
    );
}
