use std::{fs, os::unix::fs::PermissionsExt as _, path::Path};

use oxiroute_config::{ConfigDraft, HttpRouteAction, ListenerBind};
use tempfile::TempDir;
use tokio::net::{TcpListener, UnixStream};

use super::{
    Instance, echo, listener, managed_config, runtime_modes, spawn_echo_upstream, web_body,
};

fn socket_mode(path: &Path) -> u32 {
    fs::metadata(path)
        .expect("socket metadata")
        .permissions()
        .mode()
        & 0o777
}

fn with_mode(config: &mut ConfigDraft, mode: u16) {
    let ListenerBind::Unix { mode: current, .. } = &mut config.listeners[0].bind else {
        panic!("the first listener is the Unix socket")
    };
    *current = Some(mode);
}

fn with_body(config: &mut ConfigDraft, text: &str) {
    let HttpRouteAction::FixedResponse { body, .. } = &mut config.http_services[0].routes[0].action
    else {
        panic!("the fixture route is a fixed response")
    };
    *body = text.into();
}

fn unix_bind(path: &Path, mode: u16) -> ListenerBind {
    ListenerBind::Unix {
        path: path.to_owned(),
        mode: Some(mode),
    }
}

fn label(supervised: bool) -> &'static str {
    if supervised { "supervised" } else { "direct" }
}

async fn reload_changes_the_mode(initial: u16, next: u16) {
    for supervised in runtime_modes() {
        let label = label(supervised);
        let directory = TempDir::new().expect("Unix mode directory");
        let path = directory.path().join("edge.sock");
        let echo_upstream = spawn_echo_upstream();
        let mut instance = Instance::start(
            managed_config("rev-0", unix_bind(&path, initial), echo_upstream),
            supervised,
        )
        .await;
        assert_eq!(socket_mode(&path), u32::from(initial), "{label}");
        let mut established = UnixStream::connect(&path)
            .await
            .expect("established client");
        echo(&mut established, b"before the reload").await;

        let applied = instance
            .apply(|config| {
                with_mode(config, next);
                with_body(config, "rev-1");
            })
            .await;
        assert_eq!(
            applied["quarantinedRevision"],
            serde_json::Value::Null,
            "{label}"
        );
        assert_eq!(socket_mode(&path), u32::from(next), "{label}");
        assert_eq!(web_body(instance.web).await, "rev-1", "{label}");
        echo(&mut established, b"after the reload").await;
        let mut fresh = UnixStream::connect(&path).await.expect("fresh client");
        echo(&mut fresh, b"through the kept socket").await;

        drop((established, fresh));
        instance.stop();
    }
}

#[tokio::test]
async fn a_loosened_unix_socket_mode_applies_with_the_rest_of_the_reload_and_keeps_its_clients() {
    reload_changes_the_mode(0o600, 0o660).await;
}

#[tokio::test]
async fn a_tightened_unix_socket_mode_applies_with_the_rest_of_the_reload_and_keeps_its_clients() {
    reload_changes_the_mode(0o666, 0o600).await;
}

#[tokio::test]
async fn a_turned_down_reload_leaves_the_unix_socket_mode_as_it_was() {
    for supervised in runtime_modes() {
        let label = label(supervised);
        let directory = TempDir::new().expect("Unix mode directory");
        let path = directory.path().join("edge.sock");
        let mut instance = Instance::start(
            managed_config("rev-0", unix_bind(&path, 0o600), spawn_echo_upstream()),
            supervised,
        )
        .await;
        let occupied = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("occupied address");
        let address = occupied.local_addr().expect("occupied address");

        instance
            .rejects(|config| {
                with_mode(config, 0o666);
                config.listeners.push(listener(
                    "taken",
                    ListenerBind::Socket {
                        address,
                        ipv6_only: false,
                    },
                ));
            })
            .await;
        assert_eq!(socket_mode(&path), 0o600, "{label}");
        let mut client = UnixStream::connect(&path)
            .await
            .expect("client after the refusal");
        echo(&mut client, b"unchanged").await;
        assert_eq!(web_body(instance.web).await, "rev-0", "{label}");

        drop((occupied, client));
        instance.stop();
    }
}
