#[allow(clippy::wildcard_imports)]
use super::*;

#[test]
#[allow(clippy::too_many_lines)]
pub(super) fn supervised_worker_replaces_an_active_udp_session_without_rebinding_or_leaking() {
    let directory = tempfile::tempdir().expect("supervised UDP replacement directory");
    let old_upstream = StdUdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).expect("old UDP upstream");
    old_upstream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .expect("old UDP upstream timeout");
    let new_upstream = StdUdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).expect("new UDP upstream");
    new_upstream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .expect("new UDP upstream timeout");
    let listener_address = StdUdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
        .expect("UDP listener probe")
        .local_addr()
        .expect("UDP listener address");
    let mut initial = udp_only_config(
        listener_address,
        old_upstream.local_addr().expect("old UDP upstream address"),
    );
    initial.l4_services[0].idle_timeout_ms = 30_000;
    let mut candidate = initial.clone();
    candidate.upstream_pools[0].endpoints = vec![UpstreamEndpoint::Socket {
        address: new_upstream.local_addr().expect("new UDP upstream address"),
    }];
    let path = directory.path().join("oxiroute.kdl");
    fs::write(
        &path,
        render_config(
            ConfigFormat::Kdl,
            &initial
                .clone()
                .validate()
                .expect("valid initial UDP config"),
        ),
    )
    .expect("write initial UDP config");
    let mut harness = Harness::launch_at(&path, canonical_revision(&path), false);
    harness.poll_until(MasterState::Running);
    let baseline_descriptors = matching_descriptor_count(&harness.listener_targets);
    assert_eq!(baseline_descriptors, 2);

    let client = StdUdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).expect("active UDP client");
    client
        .set_read_timeout(Some(Duration::from_secs(2)))
        .expect("active UDP client timeout");
    client
        .send_to(b"old-generation", listener_address)
        .expect("send active UDP datagram");
    let mut buffer = [0_u8; 128];
    let (length, old_peer) = old_upstream
        .recv_from(&mut buffer)
        .expect("receive active UDP datagram");
    assert_eq!(&buffer[..length], b"old-generation");
    let old_worker = harness
        .master
        .worker_id(WorkerRole::Active)
        .expect("old worker pid");

    fs::write(
        &path,
        render_config(
            ConfigFormat::Kdl,
            &candidate
                .clone()
                .validate()
                .expect("valid candidate UDP config"),
        ),
    )
    .expect("write candidate UDP config");
    replace_real_worker(
        &mut harness.master,
        &path,
        canonical_revision(&path),
        2,
        false,
    );
    let quiesce_started = Instant::now();
    loop {
        harness
            .master
            .poll(Instant::now())
            .expect("master poll during UDP quiesce");
        if matches!(harness.master.state(), MasterState::ActivatingCandidate)
            || harness
                .master
                .worker_status(WorkerRole::Active)
                .is_some_and(|status| {
                    status.lifecycle == WorkerLifecycle::Quiescing && !status.accepting
                })
        {
            break;
        }
        assert!(
            quiesce_started.elapsed() < TEST_TIMEOUT,
            "UDP replacement skipped the quiesce barrier in {:?}",
            harness.master.state()
        );
        thread::sleep(POLL_INTERVAL);
    }

    let rejected_client = StdUdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).expect("new UDP client");
    rejected_client
        .send_to(b"during-quiesce", listener_address)
        .expect("send quiescing UDP datagram");
    assert_no_udp_datagram(&old_upstream);
    assert_no_udp_datagram(&new_upstream);

    harness.poll_until(MasterState::DrainingRetired);
    assert_eq!(
        harness.master.worker_id(WorkerRole::Retired),
        Some(old_worker)
    );
    assert_eq!(
        harness.master.worker_state(WorkerRole::Retired),
        Some(oxiroute_supervisor_master::WorkerState::Running)
    );

    old_upstream
        .send_to(b"old-response", old_peer)
        .expect("finish active old UDP session");
    let (length, _) = client
        .recv_from(&mut buffer)
        .expect("receive old UDP response");
    assert_eq!(&buffer[..length], b"old-response");
    harness.poll_until(MasterState::Running);
    assert_eq!(
        harness.master.active_instance().map(InstanceId::as_str),
        Some("oxiroute-stage-2-candidate-2")
    );
    let candidate_worker = harness
        .master
        .worker_id(WorkerRole::Active)
        .expect("candidate worker pid");
    let candidate_launcher = harness
        .master
        .worker_process_group_id(WorkerRole::Active)
        .expect("candidate launcher pid");
    verify_listener_descriptors(candidate_worker, &harness.listener_targets);
    assert_eq!(
        matching_descriptor_count(&harness.listener_targets),
        baseline_descriptors
    );

    let new_client = StdUdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).expect("candidate UDP client");
    new_client
        .set_read_timeout(Some(Duration::from_secs(2)))
        .expect("candidate UDP client timeout");
    new_client
        .send_to(b"new-generation", listener_address)
        .expect("send candidate UDP datagram");
    let (length, new_peer) = new_upstream
        .recv_from(&mut buffer)
        .expect("receive candidate UDP datagram");
    assert_eq!(&buffer[..length], b"new-generation");
    new_upstream
        .send_to(b"new-response", new_peer)
        .expect("send candidate UDP response");
    let (length, _) = new_client
        .recv_from(&mut buffer)
        .expect("receive candidate UDP response");
    assert_eq!(&buffer[..length], b"new-response");

    assert_eq!(
        StdUdpSocket::bind(listener_address)
            .expect_err("master and candidate must own the UDP listener")
            .kind(),
        io::ErrorKind::AddrInUse
    );
    harness
        .master
        .shutdown(Instant::now())
        .expect("UDP replacement shutdown");
    harness.poll_until(MasterState::Stopped);
    harness.verify_reaped();
    wait_process_gone(candidate_worker);
    wait_process_gone(candidate_launcher);
    drop(harness);
    StdUdpSocket::bind(listener_address).expect("UDP listener released after master drop");
}

#[test]
pub(super) fn supervised_udp_replacement_rolls_back_after_candidate_rejects_adoption() {
    let directory = tempfile::tempdir().expect("supervised UDP rollback directory");
    let upstream = StdUdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).expect("UDP upstream");
    upstream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .expect("UDP upstream timeout");
    let listener_address = StdUdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
        .expect("UDP listener probe")
        .local_addr()
        .expect("UDP listener address");
    let config = udp_only_config(
        listener_address,
        upstream.local_addr().expect("UDP upstream address"),
    );
    let path = directory.path().join("oxiroute.kdl");
    fs::write(
        &path,
        render_config(
            ConfigFormat::Kdl,
            &config
                .clone()
                .validate()
                .expect("valid UDP rollback config"),
        ),
    )
    .expect("write UDP rollback config");
    let mut harness = Harness::launch_at(&path, canonical_revision(&path), false);
    harness.poll_until(MasterState::Running);
    let baseline_descriptors = matching_descriptor_count(&harness.listener_targets);
    let active_worker = harness
        .master
        .worker_id(WorkerRole::Active)
        .expect("active worker pid");

    replace_real_worker(&mut harness.master, &path, "0".repeat(64), 2, false);
    let candidate_worker = harness
        .master
        .worker_id(WorkerRole::Candidate)
        .expect("rollback candidate pid");
    harness.poll_until(MasterState::Running);
    assert_eq!(
        harness.master.active_instance().map(InstanceId::as_str),
        Some("oxiroute-stage-2")
    );
    assert_eq!(
        harness.master.worker_id(WorkerRole::Active),
        Some(active_worker)
    );
    assert_eq!(
        matching_descriptor_count(&harness.listener_targets),
        baseline_descriptors
    );
    wait_process_gone(candidate_worker);

    let client = StdUdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).expect("rollback UDP client");
    client
        .set_read_timeout(Some(Duration::from_secs(2)))
        .expect("rollback UDP client timeout");
    client
        .send_to(b"rollback", listener_address)
        .expect("send rollback UDP datagram");
    let mut buffer = [0_u8; 128];
    let (length, peer) = upstream
        .recv_from(&mut buffer)
        .expect("receive rollback UDP datagram");
    assert_eq!(&buffer[..length], b"rollback");
    upstream
        .send_to(b"rollback-response", peer)
        .expect("send rollback UDP response");
    let (length, _) = client
        .recv_from(&mut buffer)
        .expect("receive rollback UDP response");
    assert_eq!(&buffer[..length], b"rollback-response");

    assert_eq!(
        StdUdpSocket::bind(listener_address)
            .expect_err("rollback must retain the reserved UDP listener")
            .kind(),
        io::ErrorKind::AddrInUse
    );
    let candidate_launcher = harness
        .master
        .worker_process_group_id(WorkerRole::Active)
        .expect("active launcher pid");
    harness
        .master
        .shutdown(Instant::now())
        .expect("rollback shutdown");
    harness.poll_until(MasterState::Stopped);
    harness.verify_reaped();
    wait_process_gone(candidate_launcher);
    drop(harness);
    StdUdpSocket::bind(listener_address).expect("rollback UDP listener released");
}

#[test]
pub(super) fn supervised_worker_restarts_and_serves_new_udp_traffic_after_clean_shutdown() {
    let directory = tempfile::tempdir().expect("supervised UDP restart directory");
    let upstream = StdUdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).expect("UDP upstream");
    upstream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .expect("UDP upstream timeout");
    let listener_address = StdUdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
        .expect("UDP listener probe")
        .local_addr()
        .expect("UDP listener address");
    let config = udp_only_config(
        listener_address,
        upstream.local_addr().expect("UDP upstream address"),
    );
    let path = directory.path().join("oxiroute.kdl");
    fs::write(
        &path,
        render_config(
            ConfigFormat::Kdl,
            &config.clone().validate().expect("valid UDP restart config"),
        ),
    )
    .expect("write UDP restart config");

    let mut first = Harness::launch_at(&path, canonical_revision(&path), false);
    first.poll_until(MasterState::Running);
    let client = StdUdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).expect("first UDP client");
    client
        .set_read_timeout(Some(Duration::from_secs(2)))
        .expect("first UDP client timeout");
    client
        .send_to(b"before-restart", listener_address)
        .expect("send first UDP datagram");
    let mut buffer = [0_u8; 128];
    let (length, peer) = upstream
        .recv_from(&mut buffer)
        .expect("receive first UDP datagram");
    assert_eq!(&buffer[..length], b"before-restart");
    upstream
        .send_to(b"before-restart-response", peer)
        .expect("send first UDP response");
    let (length, _) = client
        .recv_from(&mut buffer)
        .expect("receive first UDP response");
    assert_eq!(&buffer[..length], b"before-restart-response");
    drop(client);

    first
        .master
        .shutdown(Instant::now())
        .expect("first UDP shutdown");
    first.poll_until(MasterState::Stopped);
    first.verify_reaped();
    drop(first);

    let mut restarted = Harness::launch_at(&path, canonical_revision(&path), false);
    restarted.poll_until(MasterState::Running);
    let client = StdUdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).expect("restarted UDP client");
    client
        .set_read_timeout(Some(Duration::from_secs(2)))
        .expect("restarted UDP client timeout");
    client
        .send_to(b"after-restart", listener_address)
        .expect("send restarted UDP datagram");
    let (length, peer) = upstream
        .recv_from(&mut buffer)
        .expect("receive restarted UDP datagram");
    assert_eq!(&buffer[..length], b"after-restart");
    upstream
        .send_to(b"after-restart-response", peer)
        .expect("send restarted UDP response");
    let (length, _) = client
        .recv_from(&mut buffer)
        .expect("receive restarted UDP response");
    assert_eq!(&buffer[..length], b"after-restart-response");

    restarted
        .master
        .shutdown(Instant::now())
        .expect("restarted UDP shutdown");
    restarted.poll_until(MasterState::Stopped);
    restarted.verify_reaped();
    drop(restarted);
    StdUdpSocket::bind(listener_address).expect("restarted UDP listener released");
}

#[test]
pub(super) fn supervised_worker_crash_with_active_udp_session_is_reaped_and_releases_listener() {
    let directory = tempfile::tempdir().expect("supervised UDP crash directory");
    let upstream = StdUdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).expect("UDP upstream");
    upstream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .expect("UDP upstream timeout");
    let listener_address = StdUdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
        .expect("UDP listener probe")
        .local_addr()
        .expect("UDP listener address");
    let config = udp_only_config(
        listener_address,
        upstream.local_addr().expect("UDP upstream address"),
    );
    let path = directory.path().join("oxiroute.kdl");
    fs::write(
        &path,
        render_config(
            ConfigFormat::Kdl,
            &config.clone().validate().expect("valid UDP crash config"),
        ),
    )
    .expect("write UDP crash config");
    let mut harness = Harness::launch_at(&path, canonical_revision(&path), false);
    harness.poll_until(MasterState::Running);

    let client = StdUdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).expect("active UDP client");
    client
        .send_to(b"active-before-crash", listener_address)
        .expect("send active UDP datagram");
    let mut buffer = [0_u8; 128];
    let (length, _) = upstream
        .recv_from(&mut buffer)
        .expect("receive active UDP datagram");
    assert_eq!(&buffer[..length], b"active-before-crash");

    let worker = harness
        .master
        .worker_id(WorkerRole::Active)
        .expect("active UDP worker pid");
    let status = Command::new("kill")
        .arg("-KILL")
        .arg(worker.to_string())
        .status()
        .expect("kill active UDP worker");
    assert!(status.success(), "failed to kill active UDP worker");
    harness.poll_until(MasterState::Failed);
    harness.verify_reaped();
    drop(client);
    drop(harness);
    StdUdpSocket::bind(listener_address).expect("crashed UDP listener released");
}
