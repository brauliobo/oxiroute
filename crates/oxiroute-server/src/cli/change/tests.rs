use serde_json::json;

use super::{Desired, Observation, Plan, Scope, Setting};

fn plan(scope: Scope, setting: Setting, desired: Desired) -> Plan {
    Plan::new(scope, setting, desired, vec!["web/a".to_owned()])
}

fn seen(current: &str, configured: Option<&str>) -> Observation {
    Observation {
        current: Some(current.to_owned()),
        configured: configured.map(str::to_owned),
    }
}

fn rows(plan: &Plan, observed: &[Option<Observation>]) -> String {
    plan.summary(observed).plain()
}

#[test]
fn a_value_the_target_already_has_is_unchanged() {
    let plan = plan(
        Scope::Server,
        Setting::State,
        Desired::Value("drain".into()),
    );
    assert_eq!(
        rows(&plan, &[Some(seen("drain", None))]),
        "target=web/a setting=state value=drain change=unchanged\n"
    );
}

#[test]
fn a_different_value_is_changed() {
    let plan = plan(Scope::Server, Setting::Weight, Desired::Value("5".into()));
    assert_eq!(
        rows(&plan, &[Some(seen("1", None))]),
        "target=web/a setting=weight value=5 change=changed\n"
    );
}

#[test]
fn a_target_the_inventory_did_not_show_is_reported_as_applied() {
    let plan = plan(Scope::Pool, Setting::State, Desired::Value("ready".into()));
    assert_eq!(
        rows(&plan, &[None]),
        "target=web/a setting=state value=ready change=applied\n"
    );
}

#[test]
fn resetting_to_the_configured_value_compares_against_it() {
    let plan = plan(Scope::Server, Setting::MaxConnections, Desired::Configured);
    assert_eq!(
        rows(&plan, &[Some(seen("8", Some("none")))]),
        "target=web/a setting=max_connections value=none change=changed\n"
    );
    assert_eq!(
        rows(&plan, &[Some(seen("none", Some("none")))]),
        "target=web/a setting=max_connections value=none change=unchanged\n"
    );
}

#[test]
fn resetting_a_setting_without_a_configured_value_is_applied_as_default() {
    let plan = plan(Scope::Server, Setting::Weight, Desired::Configured);
    assert_eq!(
        rows(&plan, &[Some(seen("5", None))]),
        "target=web/a setting=weight value=default change=applied\n"
    );
}

#[test]
fn server_targets_are_found_by_pool_and_name() {
    let entries = json!({ "servers": [
        { "pool": "web", "server": { "name": "a", "weight": 7 } },
        { "pool": "db", "server": { "name": "a", "weight": 1 } },
    ] });
    let entry = Scope::Server.find(&entries, "web/a").expect("web/a");
    let observation = Setting::Weight.observe(Scope::Server, entry);
    assert_eq!(observation.current.as_deref(), Some("7"));
    assert!(Scope::Server.find(&entries, "web/missing").is_none());
}

#[test]
fn a_pool_state_is_mixed_when_its_servers_disagree() {
    let entries = json!({ "pools": [{ "name": "web", "endpoints": [
        { "administrativeState": "ready" }, { "administrativeState": "drain" },
    ] }] });
    let entry = Scope::Pool.find(&entries, "web").expect("web");
    let observation = Setting::State.observe(Scope::Pool, entry);
    assert_eq!(observation.current.as_deref(), Some("mixed"));
}
