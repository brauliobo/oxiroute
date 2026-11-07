#[allow(clippy::wildcard_imports)]
use super::*;

#[test]
fn installs_a_real_ecdsa_generation_with_an_intermediate() {
    let temp = TempDir::new().unwrap();
    let group = EcGroup::from_curve_name(Nid::X9_62_PRIME256V1).unwrap();
    let root_key = PKey::from_ec_key(EcKey::generate(&group).unwrap()).unwrap();
    let root = build_certificate(
        "ECDSA Root",
        None,
        &root_key,
        &root_key,
        &[],
        false,
        true,
        false,
    );
    let intermediate_key = PKey::from_ec_key(EcKey::generate(&group).unwrap()).unwrap();
    let intermediate = build_certificate(
        "ECDSA Intermediate",
        Some(&root),
        &root_key,
        &intermediate_key,
        &[],
        false,
        true,
        false,
    );
    let leaf_key = PKey::from_ec_key(EcKey::generate(&group).unwrap()).unwrap();
    let leaf = build_certificate(
        "ecdsa.example.test",
        Some(&intermediate),
        &intermediate_key,
        &leaf_key,
        &["ecdsa.example.test"],
        false,
        false,
        true,
    );
    let files = write_identity_material(
        temp.path(),
        "ecdsa",
        &leaf,
        &[&intermediate, &root],
        &leaf_key,
    );
    let generation = CertificateGeneration::from_files(
        "ecdsa",
        &["ecdsa.example.test".into()],
        &files.chain,
        &files.key,
    )
    .unwrap();
    assert_eq!(generation.metadata().intermediate_count, 2);

    let context = SslContextBuilder::new(SslMethod::tls()).unwrap().build();
    let mut ssl = Ssl::new(&context).unwrap();
    generation.install(&mut ssl).unwrap();
    assert_eq!(
        ssl.certificate().unwrap().to_der().unwrap(),
        leaf.to_der().unwrap()
    );
    assert!(ssl.private_key().unwrap().public_eq(&leaf_key));
}

#[test]
fn atomically_rejects_a_stale_generation_publication() {
    let temp = TempDir::new().unwrap();
    let first_files = write_identity(temp.path(), "first", "shared.example.test", false);
    let second_files = write_identity(temp.path(), "second", "shared.example.test", false);
    let third_files = write_identity(temp.path(), "third", "shared.example.test", false);
    let different_sans_files =
        write_identity(temp.path(), "different-sans", "other.example.test", false);
    let first = Arc::new(
        CertificateGeneration::from_files(
            "shared",
            &["shared.example.test".into()],
            &first_files.chain,
            &first_files.key,
        )
        .unwrap(),
    );
    let second = Arc::new(
        CertificateGeneration::from_files(
            "shared",
            &["shared.example.test".into()],
            &second_files.chain,
            &second_files.key,
        )
        .unwrap(),
    );
    let third = Arc::new(
        CertificateGeneration::from_files(
            "shared",
            &["shared.example.test".into()],
            &third_files.chain,
            &third_files.key,
        )
        .unwrap(),
    );
    let active = ActiveCertificateGeneration::new(Arc::clone(&first));
    let stale = active.snapshot();

    let different_identity = Arc::new(
        CertificateGeneration::from_files(
            "different",
            &["shared.example.test".into()],
            &third_files.chain,
            &third_files.key,
        )
        .unwrap(),
    );
    let error = active
        .publish_if_current(&second, different_identity)
        .unwrap_err();
    assert_eq!(
        error,
        CertificatePublishError::IdentityMismatch {
            active_name: "shared".into(),
            replacement_name: "different".into(),
        }
    );
    assert!(Arc::ptr_eq(&active.snapshot(), &first));

    let different_sans = Arc::new(
        CertificateGeneration::from_files(
            "shared",
            &["other.example.test".into()],
            &different_sans_files.chain,
            &different_sans_files.key,
        )
        .unwrap(),
    );
    let error = active
        .publish_if_current(&different_sans, Arc::clone(&different_sans))
        .unwrap_err();
    assert_eq!(
        error,
        CertificatePublishError::DnsNamesMismatch {
            identity: "shared".into(),
            active_dns_names: vec!["shared.example.test".into()],
            replacement_dns_names: vec!["other.example.test".into()],
        }
    );
    assert!(Arc::ptr_eq(&active.snapshot(), &first));

    active
        .publish_if_current(&stale, Arc::clone(&second))
        .unwrap();
    let error = active.publish_if_current(&stale, third).unwrap_err();
    let _: &CertificatePublishError = &error;
    assert!(matches!(
        error,
        CertificatePublishError::GenerationChanged {
            expected_revision,
            active_revision,
        } if expected_revision == first.metadata().revision
            && active_revision == second.metadata().revision
    ));
    assert!(Arc::ptr_eq(&active.snapshot(), &second));
}

#[test]
fn concurrent_snapshots_observe_only_complete_published_generations() {
    const READERS: usize = 8;
    const SNAPSHOTS_PER_READER: usize = 10_000;
    const PUBLICATIONS: usize = 2_000;

    let temp = TempDir::new().unwrap();
    let first_files = write_identity(temp.path(), "race-first", "race.example.test", false);
    let second_files = write_identity(temp.path(), "race-second", "race.example.test", false);
    let first = Arc::new(
        CertificateGeneration::from_files(
            "race",
            &["race.example.test".into()],
            &first_files.chain,
            &first_files.key,
        )
        .unwrap(),
    );
    let second = Arc::new(
        CertificateGeneration::from_files(
            "race",
            &["race.example.test".into()],
            &second_files.chain,
            &second_files.key,
        )
        .unwrap(),
    );
    let active = Arc::new(ActiveCertificateGeneration::new(Arc::clone(&first)));
    let barrier = Arc::new(Barrier::new(READERS + 1));
    let mut readers = Vec::with_capacity(READERS);

    for _ in 0..READERS {
        let active = Arc::clone(&active);
        let barrier = Arc::clone(&barrier);
        let first = Arc::clone(&first);
        let second = Arc::clone(&second);
        readers.push(thread::spawn(move || {
            barrier.wait();
            for _ in 0..SNAPSHOTS_PER_READER {
                let generation = active.snapshot();
                assert!(Arc::ptr_eq(&generation, &first) || Arc::ptr_eq(&generation, &second));
            }
        }));
    }

    barrier.wait();
    for _ in 0..PUBLICATIONS {
        let current = active.snapshot();
        let replacement = if Arc::ptr_eq(&current, &first) {
            Arc::clone(&second)
        } else {
            Arc::clone(&first)
        };
        active.publish_if_current(&current, replacement).unwrap();
    }
    for reader in readers {
        reader.join().unwrap();
    }
}
