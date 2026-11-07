use std::sync::{Barrier, Mutex};

use super::*;

#[derive(Default)]
struct FakeProvider {
    created: Mutex<Vec<String>>,
    cleaned: Mutex<Vec<String>>,
}

impl Dns01Provider for FakeProvider {
    fn name(&self) -> &'static str {
        "fake"
    }

    fn create_txt_record(
        &self,
        challenge: &Dns01Challenge,
        credentials: &Dns01Credentials,
        operation: &Dns01Operation,
    ) -> Result<Dns01Record, Dns01ProviderError> {
        operation.check()?;
        assert_eq!(credentials.as_bytes(), b"provider-secret");
        self.created
            .lock()
            .expect("created lock")
            .push(challenge.record_name().into());
        Dns01Record::new(
            "fake",
            challenge.challenge_url(),
            challenge.record_name(),
            challenge.record_value().as_bytes().to_vec(),
            "record-1",
        )
    }

    fn cleanup_txt_record(
        &self,
        record: &Dns01Record,
        credentials: &Dns01Credentials,
        operation: &Dns01Operation,
    ) -> Result<(), Dns01ProviderError> {
        operation.check()?;
        assert_eq!(credentials.as_bytes(), b"provider-secret");
        self.cleaned
            .lock()
            .expect("cleaned lock")
            .push(record.provider_record_id().into());
        Ok(())
    }

    fn wait_for_propagation(
        &self,
        challenge: &Dns01Challenge,
        record: &Dns01Record,
        credentials: &Dns01Credentials,
        operation: &Dns01Operation,
    ) -> Result<(), Dns01ProviderError> {
        operation.check()?;
        assert_eq!(record.record_name(), challenge.record_name());
        assert_eq!(credentials.as_bytes(), b"provider-secret");
        Ok(())
    }
}

fn challenge() -> Dns01Challenge {
    Dns01Challenge::new(
        "*.example.test",
        "https://acme.test/challenge/1",
        "_acme-challenge.example.test",
        "txt-value",
    )
    .expect("challenge")
}

#[test]
fn allowlist_and_fake_provider_keep_exact_record_identity() {
    let provider = Arc::new(FakeProvider::default());
    let mut registry = Dns01ProviderRegistry::new(["fake".into()]).expect("registry");
    registry
        .register(FakeProvider {
            created: Mutex::new(Vec::new()),
            cleaned: Mutex::new(Vec::new()),
        })
        .expect("registered provider");
    assert!(registry.permits("fake"));
    assert!(registry.get("other").is_none());

    let credentials = Dns01Credentials::new(b"provider-secret".to_vec()).expect("credentials");
    let operation = Dns01Operation::new(Duration::from_secs(1)).expect("operation");
    let record = provider
        .create_txt_record(&challenge(), &credentials, &operation)
        .expect("create");
    assert!(record.matches(&challenge(), "fake"));
    provider
        .cleanup_txt_record(&record, &credentials, &operation)
        .expect("cleanup");
}

#[test]
fn timeout_and_cancellation_are_bounded_and_categorical() {
    assert!(matches!(
        Dns01Operation::new(Duration::ZERO),
        Err(Dns01ProviderError::InvalidOperationTimeout)
    ));
    let cancellation = Dns01Cancellation::new();
    let operation = Dns01Operation::with_cancellation(Duration::from_secs(1), cancellation.clone())
        .expect("operation");
    cancellation.cancel();
    assert_eq!(operation.check(), Err(Dns01ProviderError::Cancelled));
}

#[test]
fn cancellation_between_fast_precheck_and_wait_registration_is_not_lost() {
    let cancellation = Dns01Cancellation::new();
    let prechecked = Arc::new(Barrier::new(2));
    let register = Arc::new(Barrier::new(2));
    let worker_cancellation = cancellation.clone();
    let worker_prechecked = Arc::clone(&prechecked);
    let worker_register = Arc::clone(&register);
    let worker = std::thread::spawn(move || {
        worker_cancellation.wait_timeout_with_registration(Duration::from_mins(1), || {
            worker_prechecked.wait();
            worker_register.wait();
        })
    });
    prechecked.wait();

    cancellation.cancel();
    register.wait();

    assert!(worker.join().expect("cancellation waiter"));
}

#[test]
fn pre_cancelled_waiters_skip_wait_registration() {
    for _ in 0..2_000 {
        let cancellation = Dns01Cancellation::new();
        cancellation.cancel();

        assert!(
            cancellation.wait_timeout_with_registration(Duration::from_mins(1), || panic!(
                "pre-cancelled waiter attempted registration"
            ))
        );
    }
}

#[test]
fn credentials_challenges_and_records_never_debug_secret_values() {
    let credentials = Dns01Credentials::new(b"provider-secret".to_vec()).expect("credentials");
    let challenge = challenge();
    let record = Dns01Record::new(
        "fake",
        challenge.challenge_url(),
        challenge.record_name(),
        challenge.record_value().as_bytes().to_vec(),
        "record-1",
    )
    .expect("record");
    for debug in [
        format!("{credentials:?}"),
        format!("{challenge:?}"),
        format!("{record:?}"),
    ] {
        assert!(debug.contains("REDACTED"));
        assert!(!debug.contains("provider-secret"));
        assert!(!debug.contains("txt-value"));
    }
}

#[test]
fn unallowlisted_and_dynamic_provider_names_fail_closed() {
    let mut registry = Dns01ProviderRegistry::new(["fake".into()]).expect("registry");
    assert_eq!(registry.register(FakeProvider::default()), Ok(()));
    assert_eq!(
        registry.register(FakeProvider::default()),
        Err(Dns01ProviderError::DuplicateProvider)
    );
    assert!(matches!(
        Dns01ProviderRegistry::new(["dynamic:provider".into()]),
        Err(Dns01ProviderError::InvalidProviderName)
    ));
}
