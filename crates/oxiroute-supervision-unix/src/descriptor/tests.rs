use super::*;

fn tcp(id: u16, name: &str, address: &str) -> DescriptorSlot {
    DescriptorSlot {
        id: SlotId(id),
        role: DescriptorRole::Traffic(name.into()),
        kind: DescriptorKind::TcpListener,
        bind: Some(BindIdentity::Tcp(address.parse().expect("socket address"))),
        mode: None,
    }
}

fn unix(id: u16, name: &str, path: &str, mode: Option<u16>) -> DescriptorSlot {
    DescriptorSlot {
        id: SlotId(id),
        role: DescriptorRole::Traffic(name.into()),
        kind: DescriptorKind::UnixListener,
        bind: Some(BindIdentity::UnixPath(path.into())),
        mode,
    }
}

fn manifest(slots: Vec<DescriptorSlot>) -> DescriptorManifest {
    DescriptorManifest::new(slots).expect("valid manifest")
}

#[test]
fn identical_manifests_keep_every_socket() {
    let current = manifest(vec![
        tcp(0, "web", "127.0.0.1:8080"),
        tcp(1, "api", "127.0.0.1:8081"),
    ]);
    let diff = current.diff(&current.clone());
    assert!(diff.is_unchanged());
    assert_eq!(diff.kept, [(SlotId(0), SlotId(0)), (SlotId(1), SlotId(1))]);
}

#[test]
fn an_added_listener_is_reported_with_its_next_slot() {
    let current = manifest(vec![tcp(0, "web", "127.0.0.1:8080")]);
    let next = manifest(vec![
        tcp(0, "web", "127.0.0.1:8080"),
        tcp(1, "analysis", "127.0.0.1:8195"),
    ]);
    let diff = current.diff(&next);
    assert_eq!(diff.kept, [(SlotId(0), SlotId(0))]);
    assert_eq!(diff.added, [SlotId(1)]);
    assert!(diff.removed.is_empty());
    assert!(!diff.is_unchanged());
}

#[test]
fn a_removed_listener_is_reported_with_its_current_slot() {
    let current = manifest(vec![
        tcp(0, "web", "127.0.0.1:8080"),
        tcp(1, "analysis", "127.0.0.1:8195"),
    ]);
    let next = manifest(vec![tcp(0, "web", "127.0.0.1:8080")]);
    let diff = current.diff(&next);
    assert_eq!(diff.kept, [(SlotId(0), SlotId(0))]);
    assert_eq!(diff.removed, [SlotId(1)]);
    assert!(diff.added.is_empty());
}

#[test]
fn a_changed_address_is_one_removal_and_one_addition() {
    let current = manifest(vec![tcp(0, "web", "127.0.0.1:8080")]);
    let next = manifest(vec![tcp(0, "web", "127.0.0.1:9090")]);
    let diff = current.diff(&next);
    assert!(diff.kept.is_empty());
    assert_eq!(diff.removed, [SlotId(0)]);
    assert_eq!(diff.added, [SlotId(0)]);
}

#[test]
fn a_renamed_or_renumbered_listener_keeps_its_socket() {
    let current = manifest(vec![
        tcp(0, "web", "127.0.0.1:8080"),
        tcp(1, "api", "127.0.0.1:8081"),
    ]);
    let next = manifest(vec![
        tcp(0, "api-renamed", "127.0.0.1:8081"),
        tcp(1, "web", "127.0.0.1:8080"),
    ]);
    let diff = current.diff(&next);
    assert!(diff.is_unchanged());
    assert_eq!(diff.kept, [(SlotId(0), SlotId(1)), (SlotId(1), SlotId(0))]);
}

#[test]
fn the_same_address_with_another_socket_kind_is_a_different_socket() {
    let current = manifest(vec![tcp(0, "web", "127.0.0.1:8080")]);
    let mut udp = tcp(0, "web", "127.0.0.1:8080");
    udp.kind = DescriptorKind::DatagramListener;
    let diff = current.diff(&manifest(vec![udp]));
    assert!(diff.kept.is_empty());
    assert_eq!(diff.removed, [SlotId(0)]);
    assert_eq!(diff.added, [SlotId(0)]);
}

#[test]
fn a_unix_socket_matches_by_path_and_mode() {
    let current = manifest(vec![unix(0, "local", "/run/a.sock", Some(0o660))]);
    let same = manifest(vec![unix(5, "local", "/run/a.sock", Some(0o660))]);
    assert_eq!(current.diff(&same).kept, [(SlotId(0), SlotId(5))]);
    let other_mode = manifest(vec![unix(0, "local", "/run/a.sock", Some(0o600))]);
    let diff = current.diff(&other_mode);
    assert!(diff.kept.is_empty());
    assert_eq!(diff.removed, [SlotId(0)]);
    assert_eq!(diff.added, [SlotId(0)]);
    let other_path = manifest(vec![unix(0, "local", "/run/b.sock", Some(0o660))]);
    assert!(current.diff(&other_path).kept.is_empty());
}

#[test]
fn repeated_identities_pair_one_to_one() {
    let current = manifest(vec![
        tcp(0, "a", "127.0.0.1:8080"),
        tcp(1, "b", "127.0.0.1:8080"),
    ]);
    let next = manifest(vec![tcp(0, "a", "127.0.0.1:8080")]);
    let diff = current.diff(&next);
    assert_eq!(diff.kept, [(SlotId(0), SlotId(0))]);
    assert_eq!(diff.removed, [SlotId(1)]);
}

#[test]
fn slots_without_a_bind_match_by_role() {
    let opaque = |id, role| DescriptorSlot {
        id: SlotId(id),
        role,
        kind: DescriptorKind::Opaque,
        bind: None,
        mode: None,
    };
    let current = manifest(vec![opaque(0, DescriptorRole::State)]);
    assert!(
        current
            .diff(&manifest(vec![opaque(3, DescriptorRole::State)]))
            .is_unchanged()
    );
    assert!(
        !current
            .diff(&manifest(vec![opaque(0, DescriptorRole::Control)]))
            .is_unchanged()
    );
}

#[test]
fn changes_name_listeners_for_operators() {
    let current = manifest(vec![
        tcp(0, "web", "127.0.0.1:8080"),
        tcp(1, "old", "127.0.0.1:8081"),
        unix(2, "local", "/run/a.sock", None),
    ]);
    let next = manifest(vec![
        tcp(0, "web-renamed", "127.0.0.1:8080"),
        tcp(1, "analysis", "127.0.0.1:8195"),
        unix(2, "local", "/run/a.sock", None),
    ]);
    let changes = current.changes(&next);
    assert_eq!(changes.added, ["analysis (tcp 127.0.0.1:8195)"]);
    assert_eq!(changes.removed, ["old (tcp 127.0.0.1:8081)"]);
    assert_eq!(
        changes.kept,
        [
            "web-renamed (tcp 127.0.0.1:8080)",
            "local (unix /run/a.sock)"
        ]
    );
    assert!(changes.changes_listeners());
    assert!(!current.changes(&current.clone()).changes_listeners());
}

#[test]
fn labels_cover_every_role_and_bind_shape() {
    let slot = |role, kind, bind| DescriptorSlot {
        id: SlotId(0),
        role,
        kind,
        bind,
        mode: None,
    };
    let address = || Some(BindIdentity::Tcp("[::1]:9".parse().unwrap()));
    assert_eq!(
        slot(
            DescriptorRole::Management,
            DescriptorKind::TcpListener,
            address()
        )
        .label(),
        "@management (tcp [::1]:9)"
    );
    assert_eq!(
        slot(
            DescriptorRole::Stats(1),
            DescriptorKind::DatagramListener,
            address()
        )
        .label(),
        "@stats-1 (udp [::1]:9)"
    );
    assert_eq!(
        slot(
            DescriptorRole::StatsPage(2),
            DescriptorKind::QuicListener,
            address()
        )
        .label(),
        "@stats-page-2 (quic [::1]:9)"
    );
    assert_eq!(
        slot(
            DescriptorRole::Traffic("a".into()),
            DescriptorKind::UnixListener,
            Some(BindIdentity::UnixAbstract(b"name".to_vec())),
        )
        .label(),
        "a (unix @name)"
    );
    assert_eq!(
        slot(DescriptorRole::State, DescriptorKind::Opaque, None).label(),
        "@state"
    );
    assert_eq!(
        slot(DescriptorRole::Control, DescriptorKind::Opaque, None).label(),
        "@control"
    );
}

#[test]
fn an_empty_manifest_adds_and_removes_everything() {
    let empty = manifest(Vec::new());
    let one = manifest(vec![tcp(0, "web", "127.0.0.1:8080")]);
    assert_eq!(empty.diff(&one).added, [SlotId(0)]);
    assert_eq!(one.diff(&empty).removed, [SlotId(0)]);
    assert!(empty.diff(&empty).is_unchanged());
}
