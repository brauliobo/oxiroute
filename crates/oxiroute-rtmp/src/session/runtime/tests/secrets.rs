use super::*;

fn token_policy(parameter: &str, secret: &str) -> RtmpAccessPolicy {
    RtmpAccessPolicy::new(
        [],
        Some(RtmpTokenPolicy::stream_query(parameter, secret).expect("valid token policy")),
    )
}

fn authorized_application(
    name: &str,
    publish: RtmpAccessPolicy,
    play: RtmpAccessPolicy,
) -> RtmpApplication {
    RtmpApplication::new(name, true, false).with_authorization(
        publish,
        play,
        RtmpSessionCeilings::default(),
    )
}

#[test]
fn fingerprints_list_each_configured_key_and_never_the_secret() {
    let application = authorized_application(
        "cam",
        token_policy("token", "demo-secret-not-real"),
        token_policy("viewer", "demo-viewer-secret-not-real"),
    );
    let fingerprints = application.secret_fingerprints("live").collect::<Vec<_>>();
    assert_eq!(
        fingerprints
            .iter()
            .map(|entry| (entry.role, entry.fingerprint.as_str()))
            .collect::<Vec<_>>(),
        [
            (RtmpSecretRole::Publish, "fp-8c6959af"),
            (RtmpSecretRole::Play, "fp-5d06566e")
        ]
    );
    assert!(fingerprints.iter().all(|entry| entry.service == "live"));
    assert!(fingerprints.iter().all(|entry| entry.application == "cam"));
    assert!(!format!("{fingerprints:?}").contains("not-real"));
}

#[test]
fn applications_without_a_token_policy_have_no_fingerprints() {
    let open = RtmpApplication::new("open", true, false);
    assert_eq!(open.secret_fingerprints("live").count(), 0);
    let play_only = authorized_application(
        "play",
        RtmpAccessPolicy::default(),
        token_policy("viewer", "demo-viewer-secret-not-real"),
    );
    let fingerprints = play_only.secret_fingerprints("live").collect::<Vec<_>>();
    assert_eq!(fingerprints.len(), 1);
    assert_eq!(fingerprints[0].role, RtmpSecretRole::Play);
}

#[test]
fn a_service_lists_the_fingerprints_of_every_application() {
    let applications = ["zeta", "alpha"].map(|name| {
        authorized_application(
            name,
            token_policy("token", "demo-secret-not-real"),
            token_policy("viewer", "demo-viewer-secret-not-real"),
        )
    });
    let runtime = RtmpServicePreparation::new(
        "live",
        LiveHub::new(LiveHubLimits::default()),
        RtmpSessionPolicy::new(applications),
    )
    .start(Arc::new(RtmpRegistry::new(RtmpCapabilities {
        live_ingest: true,
        manual_recording: false,
    })))
    .expect("service runtime");
    assert_eq!(
        runtime
            .secret_fingerprints()
            .into_iter()
            .map(|entry| (entry.application, entry.role))
            .collect::<Vec<_>>(),
        [
            ("alpha".to_owned(), RtmpSecretRole::Play),
            ("alpha".to_owned(), RtmpSecretRole::Publish),
            ("zeta".to_owned(), RtmpSecretRole::Play),
            ("zeta".to_owned(), RtmpSecretRole::Publish),
        ]
    );
}
