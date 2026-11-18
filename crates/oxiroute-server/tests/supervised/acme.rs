use super::reload::{finish_reload_harness, reload_config};
use super::stores::{lock_owner, wait_for_lock_owner, worker};
#[allow(clippy::wildcard_imports)]
use super::*;

use std::os::unix::fs::OpenOptionsExt as _;

use oxiroute_config::{AcmeChallengeType, AcmeKeyType};

const STATE_LOCK: &str = ".lock";

fn acme_config(address: SocketAddr, state_root: &Path, io_timeout_ms: u64) -> ConfigDraft {
    let mut config = reload_config(&ReloadShape {
        http: &[("web", address)],
        relay: None,
        origin: None,
        body: "acme",
    });
    config.http_services[0].upstream_io_timeout_ms = io_timeout_ms;
    config.certificates = vec![Certificate {
        name: "managed".into(),
        dns_names: vec!["proxy.example.test".into()],
        source: CertificateSource::AcmeManaged {
            directory_url: "https://127.0.0.1:9/directory".into(),
            state_root: state_root.to_owned(),
            contacts: vec!["mailto:ops@example.test".into()],
            terms_agreed: true,
            challenge: AcmeChallengeType::Http01,
            key_type: AcmeKeyType::default(),
            allowed_dns_suffixes: vec!["example.test".into()],
            retained_revisions: 3,
            retention_days: 30,
            dns01: None,
        },
    }];
    config
}

fn write_acme_config(path: &Path, address: SocketAddr, state_root: &Path, io_timeout_ms: u64) {
    fs::write(
        path,
        render_config(
            ConfigFormat::Kdl,
            &acme_config(address, state_root, io_timeout_ms)
                .validate()
                .expect("valid ACME config"),
        ),
    )
    .expect("write ACME config");
}

fn launch_acme_harness(path: &Path, address: SocketAddr, state_root: &Path) -> Harness {
    write_acme_config(path, address, state_root, 120_000);
    let mut harness = Harness::launch_at(path, canonical_revision(path), false);
    harness.poll_until(MasterState::Running);
    harness
}

#[test]
fn reload_moves_managed_acme_state_to_the_new_worker() {
    let directory = tempfile::tempdir().expect("ACME fixture directory");
    let path = directory.path().join("oxiroute.kdl");
    let state_root = directory.path().join("acme");
    let address = reserve_tcp_address();
    let mut harness = launch_acme_harness(&path, address, &state_root);
    let first_worker = worker(&harness, WorkerRole::Active);
    wait_for_lock_owner(&state_root.join(STATE_LOCK), first_worker);

    write_acme_config(&path, address, &state_root, 130_000);
    harness.start_reload(&path).expect("start the reload");
    harness.wait_committed();

    let second_worker = worker(&harness, WorkerRole::Active);
    assert_ne!(second_worker, first_worker);
    wait_for_lock_owner(&state_root.join(STATE_LOCK), second_worker);
    harness.wait_retired_worker_exit();
    assert_eq!(
        lock_owner(&state_root.join(STATE_LOCK)),
        Some(second_worker)
    );
    finish_reload_harness(harness);
}

#[test]
fn a_replacement_that_cannot_claim_acme_state_rolls_back_to_the_previous_owner() {
    let directory = tempfile::tempdir().expect("ACME fixture directory");
    let path = directory.path().join("oxiroute.kdl");
    let state_root = directory.path().join("acme");
    let blocked = directory.path().join("blocked");
    fs::create_dir(&blocked).expect("blocked state root");
    fs::set_permissions(&blocked, fs::Permissions::from_mode(0o700)).expect("blocked mode");
    let foreign_lock = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(blocked.join(STATE_LOCK))
        .expect("foreign lock file");
    rustix::fs::flock(
        &foreign_lock,
        rustix::fs::FlockOperation::NonBlockingLockExclusive,
    )
    .expect("foreign owner locks the replacement state");
    let address = reserve_tcp_address();
    let mut harness = launch_acme_harness(&path, address, &state_root);
    let owner = worker(&harness, WorkerRole::Active);
    wait_for_lock_owner(&state_root.join(STATE_LOCK), owner);

    write_acme_config(&path, address, &blocked, 120_000);
    harness.start_reload(&path).expect("start the reload");
    harness.poll_events_until(|event| matches!(event, MasterEvent::RollbackCompleted { .. }));
    harness.poll_until(MasterState::Running);

    assert_eq!(worker(&harness, WorkerRole::Active), owner);
    assert_eq!(lock_owner(&state_root.join(STATE_LOCK)), Some(owner));
    drop(foreign_lock);
    finish_reload_harness(harness);
}
