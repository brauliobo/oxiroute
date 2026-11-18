use super::*;

const SCOPE: Scope<'static> = Scope::Stream {
    service: "live",
    application: "cam",
    stream: "front",
};
const NOW: u64 = 1_700_000_000_000;

#[test]
fn a_minted_ticket_verifies_until_its_expiry_instant() {
    let key = TicketKey::generate();
    let ticket = key.mint(&SCOPE, NOW + 90_000);
    assert_eq!(key.verify(&SCOPE, &ticket, NOW), Ok(()));
    assert_eq!(key.verify(&SCOPE, &ticket, NOW + 89_999), Ok(()));
    assert_eq!(
        key.verify(&SCOPE, &ticket, NOW + 90_000),
        Err(TicketError::Expired)
    );
    assert_eq!(
        key.verify(&SCOPE, &ticket, NOW + 3_600_000),
        Err(TicketError::Expired)
    );
}

#[test]
fn a_ticket_for_one_stream_fails_on_another_scope() {
    let key = TicketKey::generate();
    let ticket = key.mint(&SCOPE, NOW + 90_000);
    for other in [
        Scope::Stream {
            service: "live",
            application: "cam",
            stream: "back",
        },
        Scope::Stream {
            service: "live",
            application: "cam2",
            stream: "front",
        },
        Scope::Vod {
            service: "live",
            application: "cam",
            source: "front",
            path: "",
        },
    ] {
        assert_eq!(
            key.verify(&other, &ticket, NOW),
            Err(TicketError::Invalid),
            "{other:?}"
        );
    }
}

#[test]
fn any_altered_character_invalidates_the_ticket() {
    let key = TicketKey::generate();
    let ticket = key.mint(&SCOPE, NOW + 90_000);
    for index in 0..ticket.len() {
        let mut bytes = ticket.clone().into_bytes();
        bytes[index] = if bytes[index] == b'1' { b'2' } else { b'1' };
        let tampered = String::from_utf8(bytes).unwrap();
        assert!(
            key.verify(&SCOPE, &tampered, NOW).is_err(),
            "position {index} was accepted"
        );
    }
}

#[test]
fn extending_the_expiry_invalidates_the_ticket() {
    let key = TicketKey::generate();
    let ticket = key.mint(&SCOPE, NOW + 90_000);
    let (_, mac) = ticket.split_once('.').unwrap();
    let extended = format!("{}.{mac}", NOW + 3_600_000);
    assert_eq!(
        key.verify(&SCOPE, &extended, NOW),
        Err(TicketError::Invalid)
    );
}

#[test]
fn malformed_tickets_are_rejected() {
    let key = TicketKey::generate();
    for ticket in ["", ".", "abc", "123", "123.", ".abc", "x.y", "-1.abc"] {
        assert!(key.verify(&SCOPE, ticket, NOW).is_err(), "{ticket:?}");
    }
}

#[test]
fn tickets_do_not_verify_under_another_process_secret() {
    let ticket = TicketKey::generate().mint(&SCOPE, NOW + 90_000);
    assert_eq!(
        TicketKey::generate().verify(&SCOPE, &ticket, NOW),
        Err(TicketError::Invalid)
    );
}

#[test]
fn the_ticket_parameter_is_read_only_when_it_appears_once() {
    assert_eq!(query_ticket(None), Ok(None));
    assert_eq!(query_ticket(Some("a=1&b=2")), Ok(None));
    assert_eq!(query_ticket(Some("access_token=x")), Ok(None));
    assert_eq!(query_ticket(Some("a=1&ticket=1.x&b=2")), Ok(Some("1.x")));
    assert_eq!(
        query_ticket(Some("ticket=1.x&ticket=2.y")),
        Err(TicketError::Malformed)
    );
    assert_eq!(query_ticket(Some("tickets=1.x")), Ok(None));
}

#[test]
fn the_ready_path_carries_the_ticket_as_its_only_query() {
    assert_eq!(
        ticket_path("/api/v1/rtmp/media/live/cam/front/index.m3u8", "1.x"),
        "/api/v1/rtmp/media/live/cam/front/index.m3u8?ticket=1.x"
    );
}
