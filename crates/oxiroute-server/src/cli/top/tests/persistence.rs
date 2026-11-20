use std::fs;

use tempfile::TempDir;

use super::harness::{PLAIN, app, running};
use crate::cli::top::{
    app::Key,
    history::Window,
    layout::Tier,
    model::Units,
    prefs::{ColumnPrefs, Density, Panel, PanelState, Prefs, Store},
    theme::ThemeName,
    views::View,
};

fn store(directory: &TempDir) -> Store {
    Store::at(directory.path().join("oxiroute").join("top.json"))
}

fn customised() -> Prefs {
    let mut prefs = Prefs::default();
    prefs.panels.set(Panel::Summary, PanelState::Hidden);
    prefs.panels.set(Panel::Meters, PanelState::Collapsed);
    prefs.views.toggle_hidden(View::Rtmp);
    prefs.views.shift(View::Clients, -1);
    prefs.edit_columns(View::Upstreams, |columns| {
        let health = View::Upstreams.column_named("health").expect("column");
        columns.toggle(View::Upstreams, health, Tier::Wide);
        columns.shift(View::Upstreams, 5, 2);
    });
    prefs.density = Density::Compact;
    prefs.theme = ThemeName::Light;
    prefs.window = Window::Long;
    prefs.units = Units::Bits;
    prefs
}

#[test]
fn the_layout_survives_a_save_and_a_load() {
    let directory = TempDir::new().expect("directory");
    let store = store(&directory);
    assert_eq!(store.load().expect("defaults"), Prefs::default());
    let prefs = customised();
    store.save(&prefs).expect("save");
    assert_eq!(store.load().expect("load"), prefs);
    assert!(
        !directory
            .path()
            .join("oxiroute")
            .join("top.json.tmp")
            .exists()
    );
}

#[test]
fn the_saved_file_names_choices_only() {
    let directory = TempDir::new().expect("directory");
    let store = store(&directory);
    store.save(&customised()).expect("save");
    let text =
        fs::read_to_string(directory.path().join("oxiroute").join("top.json")).expect("file");
    assert!(text.ends_with('\n') && !text.contains('\r'));
    let document: serde_json::Value = serde_json::from_str(&text).expect("json");
    let mut keys = document
        .as_object()
        .expect("object")
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    keys.sort();
    assert_eq!(
        keys,
        [
            "columns", "density", "panels", "theme", "units", "version", "views", "window"
        ]
    );
}

#[test]
fn a_choice_made_in_the_dashboard_is_what_the_next_run_starts_with() {
    let directory = TempDir::new().expect("directory");
    let store = store(&directory);
    let mut first = running(View::Listeners);
    for key in ['H', 'd', 'u', 'T', 'w'] {
        first.key(Key::Char(key));
    }
    assert!(first.prefs_dirty);
    store.save(&first.prefs).expect("save");
    let loaded = store.load().expect("load");
    assert_eq!(loaded.window, Window::Medium);
    let second = app(View::Listeners, loaded, PLAIN);
    assert_eq!(second.prefs.panels.meters, PanelState::Hidden);
    assert_eq!(second.prefs.density, Density::Compact);
    assert_eq!(second.prefs.units, Units::Bits);
    assert_eq!(second.prefs.theme, ThemeName::Light);
}

#[test]
fn a_reset_forgets_the_layout() {
    let directory = TempDir::new().expect("directory");
    let store = store(&directory);
    store.save(&customised()).expect("save");
    store.reset().expect("reset");
    assert_eq!(store.load().expect("defaults"), Prefs::default());
    store.reset().expect("a second reset has nothing to remove");
}

#[test]
fn a_damaged_or_foreign_file_is_refused_with_the_way_out() {
    let directory = TempDir::new().expect("directory");
    let store = store(&directory);
    fs::create_dir_all(directory.path().join("oxiroute")).expect("directory");
    let path = directory.path().join("oxiroute").join("top.json");
    for contents in ["not json", r#"{"version": 9}"#] {
        fs::write(&path, contents).expect("write");
        let message = store.load().expect_err("refused").to_string();
        assert!(message.contains("--reset-layout"), "{message}");
    }
    fs::write(&path, vec![b' '; 70_000]).expect("write");
    assert!(
        store
            .load()
            .expect_err("too large")
            .to_string()
            .contains("64 KiB")
    );
}

#[test]
fn unknown_or_missing_fields_fall_back_to_the_defaults() {
    let directory = TempDir::new().expect("directory");
    let store = store(&directory);
    fs::create_dir_all(directory.path().join("oxiroute")).expect("directory");
    fs::write(
        directory.path().join("oxiroute").join("top.json"),
        r#"{"version": 1, "density": "compact", "panels": {"summary": "hidden"}}"#,
    )
    .expect("write");
    let prefs = store.load().expect("load");
    assert_eq!(prefs.density, Density::Compact);
    assert_eq!(prefs.panels.summary, PanelState::Hidden);
    assert_eq!(prefs.panels.meters, PanelState::Expanded);
    assert_eq!(prefs.panels.drawer, PanelState::Collapsed);
    assert_eq!(prefs.columns_of(View::Listeners), ColumnPrefs::default());
}

#[test]
fn the_file_lives_under_the_xdg_state_directory() {
    let place = |variables: &'static [(&'static str, &'static str)]| {
        Store::standard(&|name| {
            variables
                .iter()
                .find(|(candidate, _)| *candidate == name)
                .map(|(_, value)| (*value).to_owned())
        })
    };
    let xdg = place(&[("XDG_STATE_HOME", "/state"), ("HOME", "/home/alice")]).expect("store");
    assert_eq!(
        format!("{xdg:?}"),
        r#"Store { path: "/state/oxiroute/top.json" }"#
    );
    let home = place(&[("HOME", "/home/alice")]).expect("store");
    assert_eq!(
        format!("{home:?}"),
        r#"Store { path: "/home/alice/.local/state/oxiroute/top.json" }"#
    );
    assert!(
        place(&[])
            .expect_err("nowhere to put it")
            .to_string()
            .contains("XDG_STATE_HOME")
    );
}
