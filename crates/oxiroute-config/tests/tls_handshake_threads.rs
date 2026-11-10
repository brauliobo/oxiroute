use oxiroute_config::{ConfigDraft, ConfigError};
use serde_json::{Value, json};

fn validate(extra: &Value) -> Result<oxiroute_config::ValidatedConfig, ConfigError> {
    let mut config = json!({"version": 1, "listeners": []});
    config
        .as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    serde_json::from_value::<ConfigDraft>(config)
        .expect("authored configuration")
        .validate()
}

#[test]
fn absence_is_automatic_and_stays_out_of_the_canonical_form() {
    let config = validate(&json!({})).unwrap();

    assert_eq!(config.as_draft().tls_handshake_threads, None);
    let canonical = serde_json::to_value(&config).unwrap();
    assert!(canonical.get("tls_handshake_threads").is_none());
}

#[test]
fn automatic_is_one_thread_per_eight_cpus_between_one_and_four() {
    let draft = validate(&json!({})).unwrap();
    let draft = draft.as_draft();
    let threads = |cpus| draft.effective_tls_handshake_threads(cpus);

    assert_eq!(
        [threads(1), threads(8), threads(9), threads(16)],
        [1, 1, 2, 2]
    );
    assert_eq!([threads(24), threads(32), threads(256)], [3, 4, 4]);
}

#[test]
fn zero_handshakes_inline_and_a_count_is_taken_as_written() {
    let inline = validate(&json!({"tls_handshake_threads": 0})).unwrap();
    assert_eq!(inline.as_draft().effective_tls_handshake_threads(64), 0);

    for threads in [1, 6, ConfigDraft::MAX_TLS_HANDSHAKE_THREADS] {
        let config = validate(&json!({"tls_handshake_threads": threads})).unwrap();
        assert_eq!(config.as_draft().tls_handshake_threads, Some(threads));
        assert_eq!(
            config.as_draft().effective_tls_handshake_threads(2),
            usize::try_from(threads).unwrap()
        );
    }
}

#[test]
fn the_count_is_bounded() {
    let too_many = ConfigDraft::MAX_TLS_HANDSHAKE_THREADS + 1;
    let error = validate(&json!({"tls_handshake_threads": too_many})).unwrap_err();
    assert!(
        matches!(error, ConfigError::InvalidTlsHandshakeThreads(value) if value == too_many),
        "{error}"
    );
}
