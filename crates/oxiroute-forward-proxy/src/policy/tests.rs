use super::*;

fn context() -> PolicyContext {
    PolicyContext {
        protocol: Protocol::Http1,
        principal: Principal::new("test"),
    }
}

#[test]
fn denies_private_or_mixed_dns_answers() {
    let destination = Destination {
        host: Host::Dns("example.com".into()),
        port: 443,
    };
    let policy = ForbiddenDestinationPolicy;
    assert_eq!(
        policy.authorize(
            &context(),
            &destination,
            &[
                "93.184.216.34".parse().unwrap(),
                "127.0.0.1".parse().unwrap()
            ]
        ),
        Err(PolicyError::ForbiddenAddress)
    );
}

#[test]
fn permits_public_addresses() {
    let destination = Destination {
        host: Host::Ip("2606:4700:4700::1111".parse().unwrap()),
        port: 443,
    };
    assert!(
        ForbiddenDestinationPolicy
            .authorize(
                &context(),
                &destination,
                &["2606:4700:4700::1111".parse().unwrap()]
            )
            .is_ok()
    );
}

#[test]
fn empty_allow_lists_permit_anonymous_public_destinations() {
    let policy = DestinationRules::new([], [], [], [], true).expect("public policy");
    let destination = Destination {
        host: Host::Dns("example.com".into()),
        port: 80,
    };

    assert!(
        policy
            .authorize(
                &context(),
                &destination,
                &["93.184.216.34".parse().unwrap()]
            )
            .is_ok()
    );
    assert_eq!(
        policy.authorize(
            &context(),
            &destination,
            &[
                "93.184.216.34".parse().unwrap(),
                "192.0.2.254".parse().unwrap()
            ]
        ),
        Err(PolicyError::ForbiddenAddress)
    );
}

#[test]
fn deny_rules_override_and_allow_rules_constrain_the_complete_answer() {
    let policy = DestinationRules::new(
        ["*.example.com".into()],
        ["blocked.example.com".into()],
        ["93.184.216.0/24".into()],
        ["93.184.216.128/25".into()],
        false,
    )
    .expect("bounded policy");
    let destination = |name: &str| Destination {
        host: Host::Dns(name.into()),
        port: 443,
    };

    assert!(
        policy
            .authorize(
                &context(),
                &destination("www.example.com"),
                &["93.184.216.34".parse().unwrap()]
            )
            .is_ok()
    );
    assert_eq!(
        policy.authorize(
            &context(),
            &destination("blocked.example.com"),
            &["93.184.216.34".parse().unwrap()]
        ),
        Err(PolicyError::Rejected)
    );
    assert_eq!(
        policy.authorize(
            &context(),
            &destination("deep.www.example.com"),
            &["93.184.216.34".parse().unwrap()]
        ),
        Err(PolicyError::Rejected)
    );
    assert_eq!(
        policy.authorize(
            &context(),
            &destination("www.example.com"),
            &[
                "93.184.216.34".parse().unwrap(),
                "93.184.217.1".parse().unwrap()
            ]
        ),
        Err(PolicyError::Rejected)
    );
    assert_eq!(
        policy.authorize(
            &context(),
            &destination("www.example.com"),
            &["93.184.216.200".parse().unwrap()]
        ),
        Err(PolicyError::ForbiddenAddress)
    );
}

#[test]
fn domain_allow_rules_reject_ip_literals_and_cidr_rules_are_family_exact() {
    let policy = DestinationRules::new(
        ["example.com".into()],
        [],
        ["2001:db9::/32".into()],
        [],
        false,
    )
    .expect("dual constraint policy");
    assert_eq!(
        policy.authorize(
            &context(),
            &Destination {
                host: Host::Ip("2001:db9::1".parse().unwrap()),
                port: 443,
            },
            &["2001:db9::1".parse().unwrap()]
        ),
        Err(PolicyError::Rejected)
    );
    assert_eq!(
        policy.authorize(
            &context(),
            &Destination {
                host: Host::Dns("example.com".into()),
                port: 443,
            },
            &["93.184.216.34".parse().unwrap()]
        ),
        Err(PolicyError::Rejected)
    );
}

#[test]
fn malformed_duplicate_and_host_bit_rules_are_rejected() {
    assert_eq!(
        DestinationRules::new(["bad..name".into()], [], [], [], true),
        Err(RuleError::Malformed)
    );
    assert_eq!(
        DestinationRules::new(
            ["EXAMPLE.COM".into(), "example.com".into()],
            [],
            [],
            [],
            true
        ),
        Err(RuleError::Duplicate)
    );
    assert_eq!(
        DestinationRules::new([], [], ["192.0.2.1/24".into()], [], true),
        Err(RuleError::Malformed)
    );
    assert_eq!(TimeWindow::new(0, 0, 60), Err(RuleError::InvalidTimeWindow));
    assert_eq!(
        TimeWindow::new(1, 60, 60),
        Err(RuleError::InvalidTimeWindow)
    );
}

#[test]
fn time_windows_are_utc_bounded_and_deny_overrides() {
    let policy = DestinationRules::new([], [], [], [], false)
        .expect("base policy")
        .with_time_windows(
            [TimeWindow::new(1, 9 * 60, 17 * 60).expect("Monday window")],
            [TimeWindow::new(1, 12 * 60, 13 * 60).expect("Monday lunch deny")],
        )
        .expect("time policy");
    let destination = Destination {
        host: Host::Dns("example.com".into()),
        port: 443,
    };
    let monday_morning = UNIX_EPOCH + Duration::from_hours(106);
    let monday_lunch = UNIX_EPOCH + Duration::from_hours(108);
    let tuesday_morning = UNIX_EPOCH + Duration::from_hours(130);

    assert!(
        policy
            .authorize_at(
                &context(),
                &destination,
                &["93.184.216.34".parse().unwrap()],
                monday_morning,
            )
            .is_ok()
    );
    assert_eq!(
        policy.authorize_at(
            &context(),
            &destination,
            &["93.184.216.34".parse().unwrap()],
            monday_lunch,
        ),
        Err(PolicyError::ForbiddenTime)
    );
    assert_eq!(
        policy.authorize_at(
            &context(),
            &destination,
            &["93.184.216.34".parse().unwrap()],
            tuesday_morning,
        ),
        Err(PolicyError::ForbiddenTime)
    );
}

#[test]
fn approved_destination_retains_only_the_checked_addresses() {
    let policy = DestinationRules::new([], [], [], [], false).expect("base policy");
    let destination = Destination {
        host: Host::Dns("example.com".into()),
        port: 443,
    };
    let addresses = vec!["93.184.216.34".parse().unwrap()];
    let approved = policy
        .approve(&context(), &destination, &addresses)
        .expect("approved destination");

    assert_eq!(approved.destination(), &destination);
    assert_eq!(
        approved.socket_addresses(),
        &["93.184.216.34:443".parse().unwrap()]
    );
}
