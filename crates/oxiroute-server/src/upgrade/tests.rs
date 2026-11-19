use std::{os::linux::net::SocketAddrExt as _, path::PathBuf};

use super::*;

fn name_of(path: &std::path::Path) -> Vec<u8> {
    control_address(path)
        .expect("address")
        .as_abstract_name()
        .expect("an abstract name")
        .to_vec()
}

#[test]
fn every_configuration_has_its_own_stable_address() {
    let directory = tempfile::tempdir().expect("directory");
    let first = directory.path().join("first.kdl");
    let second = directory.path().join("second.kdl");
    std::fs::write(&first, "").expect("first");
    std::fs::write(&second, "").expect("second");

    assert_eq!(name_of(&first), name_of(&first));
    assert_ne!(name_of(&first), name_of(&second));
    let relative = directory.path().join(".").join("first.kdl");
    assert_eq!(name_of(&relative), name_of(&first));
}

#[test]
fn an_unreadable_configuration_has_no_address() {
    let missing = PathBuf::from("/nonexistent/oxiroute.kdl");
    assert!(control_address(&missing).is_err());
}

#[test]
fn events_are_one_json_object_each() {
    let events = [
        Event::Starting {
            binary: PathBuf::from("/usr/bin/oxiroute"),
            build_id: "gnu:ab".into(),
            previous_build_id: "gnu:cd".into(),
        },
        Event::Serving { pid: 7 },
        Event::Drained,
        Event::Refused,
        Event::Failed {
            reason: "not ready".into(),
        },
    ];
    for event in events {
        let line = serde_json::to_string(&event).expect("encode");
        assert!(!line.contains('\n'));
        let decoded: Event = serde_json::from_str(&line).expect("decode");
        assert_eq!(serde_json::to_string(&decoded).expect("encode again"), line);
    }
}

#[test]
fn a_command_without_a_binary_names_the_masters_own() {
    let command: Command = serde_json::from_str("{}").expect("decode");
    assert!(command.binary.is_none());
}
