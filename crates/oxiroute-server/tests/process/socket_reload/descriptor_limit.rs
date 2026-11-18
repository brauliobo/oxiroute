use std::{
    fs,
    net::{Ipv4Addr, SocketAddr, TcpListener},
    path::Path,
    sync::atomic::{AtomicU16, Ordering},
};

use oxiroute_config::{ConfigDraft, HttpRouteAction, ListenerBind, Management};
use tempfile::TempDir;

use super::{
    Instance, SUPERVISED_LAUNCHER, config_support::empty_config, fixed_service, listener,
    process_support, reserve_tcp_address, runtime_modes, web_body,
};

const FIRST_PORT: u16 = 38_000;
const LAST_PORT: u16 = 38_999;

static NEXT_PORT: AtomicU16 = AtomicU16::new(0);

fn free_addresses(count: usize) -> Vec<SocketAddr> {
    let span = LAST_PORT - FIRST_PORT + 1;
    let start = NEXT_PORT.fetch_add(300, Ordering::Relaxed);
    let offset = u16::try_from(std::process::id() % u32::from(span)).expect("bounded offset");
    let held = (0..span)
        .map(|step| FIRST_PORT + (offset + start + step) % span)
        .filter_map(|port| TcpListener::bind((Ipv4Addr::LOCALHOST, port)).ok())
        .take(count)
        .collect::<Vec<_>>();
    assert_eq!(held.len(), count, "free loopback ports for the test");
    held.iter()
        .map(|socket| socket.local_addr().expect("held address"))
        .collect()
}

fn many_listeners(addresses: &[SocketAddr], body: &str) -> (ConfigDraft, SocketAddr) {
    let management = reserve_tcp_address();
    let mut config = empty_config();
    config.management = Some(Management {
        bind: management,
        ui_dir: None,
        token_file: None,
    });
    config.listeners = addresses
        .iter()
        .enumerate()
        .map(|(index, address)| {
            listener(
                &format!("listener-{index}"),
                ListenerBind::Socket {
                    address: *address,
                    ipv6_only: false,
                },
            )
        })
        .collect();
    config.http_services = vec![fixed_service(body)];
    (config, management)
}

fn child_processes(pid: u32) -> usize {
    fs::read_dir(format!("/proc/{pid}/task"))
        .expect("server tasks")
        .filter_map(Result::ok)
        .filter_map(|task| fs::read_to_string(task.path().join("children")).ok())
        .map(|children| children.split_whitespace().count())
        .sum()
}

async fn assert_all_serve(addresses: &[SocketAddr], body: &str, label: &str) {
    for address in addresses {
        assert_eq!(web_body(*address).await, body, "{label}: {address}");
    }
}

#[tokio::test]
async fn more_than_sixty_four_listeners_start_and_serve_in_both_runtimes() {
    for supervised in runtime_modes() {
        let label = if supervised { "supervised" } else { "direct" };
        let addresses = free_addresses(70);
        let (config, management) = many_listeners(&addresses, "rev-0");
        let instance = Instance::start((config, management, addresses[0]), supervised).await;
        if supervised {
            assert!(
                child_processes(instance.server.pid()) > 0,
                "the master runs a worker"
            );
        }
        assert_all_serve(&addresses, "rev-0", label).await;
        instance.stop();
    }
}

#[tokio::test]
async fn a_reload_can_grow_past_sixty_four_listeners() {
    for supervised in runtime_modes() {
        let label = if supervised { "supervised" } else { "direct" };
        let addresses = free_addresses(70);
        let (config, management) = many_listeners(&addresses[..2], "rev-0");
        let mut instance = Instance::start((config, management, addresses[0]), supervised).await;

        let grown = many_listeners(&addresses, "rev-1").0;
        let applied = instance
            .apply(|config| {
                config.listeners = grown.listeners;
                let HttpRouteAction::FixedResponse { body, .. } =
                    &mut config.http_services[0].routes[0].action
                else {
                    panic!("the fixture route is a fixed response")
                };
                *body = "rev-1".into();
            })
            .await;
        assert_eq!(applied["lastRejection"], serde_json::Value::Null, "{label}");
        assert_all_serve(&addresses, "rev-1", label).await;
        instance.stop();
    }
}

#[test]
fn more_listeners_than_a_worker_can_take_stop_the_service_instead_of_running_direct() {
    if !Path::new(SUPERVISED_LAUNCHER).is_file() {
        eprintln!("the supervised launcher is not installed: nothing to refuse");
        return;
    }
    let directory = TempDir::new().expect("oversized configuration directory");
    let addresses = free_addresses(260);
    let (config, _) = many_listeners(&addresses, "rev-0");
    let path = directory.path().join("oxiroute.lua");
    process_support::write_config(&path, &config.validate().expect("valid oversized config"));

    let failure = process_support::run_to_failure(&path, None);

    assert!(!failure.status.success());
    let stderr = String::from_utf8_lossy(&failure.stderr);
    assert!(
        stderr.contains("at most 253 listener descriptors"),
        "{stderr}"
    );
}
