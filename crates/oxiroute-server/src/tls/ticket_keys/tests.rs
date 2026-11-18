use super::*;

fn force_rotation(keys: &TicketKeys) {
    keys.rotation.lock().expect("rotation").switch_at = 0;
}

#[test]
fn a_ticket_decrypts_to_what_was_encrypted() {
    let keys = TicketKeys::for_profile("round-trip");

    let ticket = keys.encrypt(b"session").expect("ticket");

    assert_eq!(keys.decrypt(&ticket).as_deref(), Some(&b"session"[..]));
}

#[test]
fn a_tampered_or_foreign_ticket_is_refused() {
    let keys = TicketKeys::for_profile("tampered");
    let other = TicketKeys::for_profile("tampered-other");
    let mut ticket = keys.encrypt(b"session").expect("ticket");

    assert_eq!(other.decrypt(&ticket), None);
    let last = ticket.len() - 1;
    ticket[last] ^= 1;
    assert_eq!(keys.decrypt(&ticket), None);
    assert_eq!(keys.decrypt(b"short"), None);
}

#[test]
fn the_generations_of_a_profile_in_one_process_share_their_keys() {
    let first = TicketKeys::for_profile("shared");
    let ticket = first.encrypt(b"session").expect("ticket");

    let second = TicketKeys::for_profile("shared");

    assert_eq!(second.decrypt(&ticket).as_deref(), Some(&b"session"[..]));
}

#[test]
fn a_profile_without_a_running_generation_starts_with_new_keys() {
    let ticket = TicketKeys::for_profile("gone")
        .encrypt(b"session")
        .expect("ticket");

    assert_eq!(TicketKeys::for_profile("gone").decrypt(&ticket), None);
}

#[test]
fn a_replacement_process_decrypts_the_tickets_of_the_process_it_replaces() {
    let old = TicketKeys::for_profile("exported");
    let ticket = old.encrypt(b"session").expect("ticket");
    let state = TicketKeys::export_all()
        .into_iter()
        .find(|state| state.profile == "exported")
        .expect("state of the running profile");
    let json = serde_json::to_vec(&state).expect("serialized");
    let state: TicketKeysState = serde_json::from_slice(&json).expect("deserialized");

    drop(old);
    let replacement = TicketKeys::for_profile("exported");
    assert_eq!(replacement.decrypt(&ticket), None);
    TicketKeys::import_all(&[state]);

    assert_eq!(
        replacement.decrypt(&ticket).as_deref(),
        Some(&b"session"[..])
    );
}

#[test]
fn rotation_keeps_the_previous_keys_for_one_more_lifetime() {
    let keys = TicketKeys::for_profile("rotated");
    let ticket = keys.encrypt(b"session").expect("ticket");
    force_rotation(&keys);

    let fresh = keys.encrypt(b"later").expect("ticket");

    assert_eq!(keys.decrypt(&ticket).as_deref(), Some(&b"session"[..]));
    assert_eq!(keys.decrypt(&fresh).as_deref(), Some(&b"later"[..]));
    force_rotation(&keys);
    assert_eq!(keys.decrypt(&ticket), None);
    assert_eq!(keys.decrypt(&fresh).as_deref(), Some(&b"later"[..]));
}

#[test]
fn state_of_a_profile_this_process_does_not_run_is_left_out() {
    let state = TicketKeysState {
        profile: "not-running".into(),
        current: Key::generate().export(),
        previous: None,
        switch_at: u64::MAX,
    };

    TicketKeys::import_all(&[state]);

    assert!(
        TicketKeys::export_all()
            .iter()
            .all(|state| state.profile != "not-running")
    );
}
