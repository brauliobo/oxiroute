use ratatui::{
    crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind},
    layout::Rect,
};

use super::{
    fixture::{load, sample, snapshot},
    harness::running,
};
use crate::cli::top::{
    app::{Action, App, Key, Layer},
    layout::{Drawer, Meters, Summary, Tier},
    prefs::{ColumnPrefs, Density, Panel, PanelState, ViewPrefs},
    views::View,
};

const WIDE: Rect = Rect::new(0, 0, 120, 30);
const COMPACT: Rect = Rect::new(0, 0, 80, 24);

fn click(app: &mut App, rect: Rect) {
    app.mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: rect.x + rect.width / 2,
        row: rect.y,
        modifiers: KeyModifiers::NONE,
    });
}

fn press(app: &mut App, keys: &str) {
    for key in keys.chars() {
        app.key(Key::Char(key));
    }
}

fn refresh(app: &mut App) {
    app.sample(Ok(sample(snapshot(14_000, 104_000, &load(4_000, 40, 60)))));
}

fn names(view: View, indexes: &[usize]) -> Vec<&'static str> {
    indexes
        .iter()
        .map(|index| view.columns()[*index].title)
        .collect()
}

#[test]
fn panels_collapse_hide_and_come_back_by_key() {
    let mut app = running(View::Listeners);
    app.prepare(WIDE);
    let layout = app.scene.layout.expect("layout");
    assert!(matches!(layout.meters, Some(Meters::Boxes(_))));
    assert!(matches!(layout.summary, Some(Summary::Box(_))));
    press(&mut app, "h");
    assert_eq!(app.prefs.panels.meters, PanelState::Collapsed);
    app.prepare(WIDE);
    assert!(matches!(
        app.scene.layout.expect("layout").meters,
        Some(Meters::Title(_))
    ));
    press(&mut app, "h");
    assert_eq!(app.prefs.panels.meters, PanelState::Expanded);
    press(&mut app, "H");
    app.prepare(WIDE);
    assert_eq!(app.prefs.panels.meters, PanelState::Hidden);
    assert!(app.scene.layout.expect("layout").meters.is_none());
    press(&mut app, "H");
    assert_eq!(app.prefs.panels.meters, PanelState::Expanded);
    assert!(app.prefs_dirty);
}

#[test]
fn a_hidden_panel_is_listed_in_the_footer_and_restored_by_clicking_it() {
    let mut app = running(View::Listeners);
    press(&mut app, "S");
    app.prepare(WIDE);
    assert!(app.scene.layout.expect("layout").summary.is_none());
    let chip = app
        .scene
        .chips
        .iter()
        .find(|chip| chip.restore)
        .expect("a restore chip");
    assert_eq!(chip.action, Action::RestorePanel(Panel::Summary));
    let rect = chip.rect;
    click(&mut app, rect);
    assert_eq!(app.prefs.panels.summary, PanelState::Expanded);
    app.prepare(WIDE);
    assert!(app.scene.chips.iter().all(|chip| !chip.restore));
}

#[test]
fn clicking_a_panel_title_collapses_it() {
    let mut app = running(View::Listeners);
    app.prepare(WIDE);
    let (rect, panel) = app.scene.titles[0];
    assert_eq!(panel, Panel::Meters);
    click(&mut app, rect);
    assert_eq!(app.prefs.panels.meters, PanelState::Collapsed);
}

#[test]
fn the_drawer_sits_beside_the_table_when_wide_and_below_it_otherwise() {
    let mut app = running(View::Listeners);
    app.prepare(WIDE);
    press(&mut app, "i");
    app.prepare(WIDE);
    assert!(matches!(
        app.scene.layout.expect("layout").drawer,
        Some(Drawer::Side(_))
    ));
    app.prepare(Rect::new(0, 0, 100, 30));
    assert!(matches!(
        app.scene.layout.expect("layout").drawer,
        Some(Drawer::Bottom(_))
    ));
    press(&mut app, "I");
    app.prepare(WIDE);
    assert!(app.scene.layout.expect("layout").drawer.is_none());
    assert!(!app.layers.contains(&Layer::Drawer));
    press(&mut app, "i");
    assert!(app.layers.is_empty());
}

#[test]
fn compact_density_and_focus_leave_only_the_table() {
    let mut app = running(View::Listeners);
    press(&mut app, "d");
    app.prepare(WIDE);
    let layout = app.scene.layout.expect("layout");
    assert_eq!(app.prefs.density, Density::Compact);
    assert!(layout.meters.is_none() && layout.summary.is_none() && !layout.bordered);
    press(&mut app, "d");
    press(&mut app, "f");
    app.prepare(WIDE);
    let layout = app.scene.layout.expect("layout");
    assert!(layout.status.is_none() && layout.meters.is_none() && layout.drawer.is_none());
    assert!(layout.table.height > 25);
}

#[test]
fn the_default_columns_depend_on_the_width_tier() {
    let prefs = ColumnPrefs::default();
    let at = |tier| names(View::Listeners, &prefs.effective(View::Listeners, tier));
    let minimal = at(Tier::Minimal);
    assert_eq!(
        minimal,
        [
            "NAME", "STATE", "CONN", "REQ/s", "IN/s", "OUT/s", "ERR/s", "P99"
        ]
    );
    assert_eq!(at(Tier::Compact), minimal);
    let standard = at(Tier::Standard);
    assert!(
        standard.contains(&"BIND") && standard.contains(&"ACC/s") && !standard.contains(&"P50")
    );
    let wide = at(Tier::Wide);
    assert!(wide.contains(&"PROTO") && wide.contains(&"P50") && wide.contains(&"CACHE"));
    assert!(!wide.contains(&"MAX"));
}

#[test]
fn columns_toggle_and_reorder_but_the_label_stays_first() {
    let view = View::Listeners;
    let mut prefs = ColumnPrefs::default();
    let req = view.column_named("req").expect("column");
    let max = view.column_named("max").expect("column");
    prefs.toggle(view, req, Tier::Wide);
    prefs.toggle(view, max, Tier::Wide);
    prefs.toggle(view, 0, Tier::Wide);
    let shown = names(view, &prefs.effective(view, Tier::Wide));
    assert!(!shown.contains(&"REQ/s") && shown.contains(&"MAX") && shown[0] == "NAME");
    assert!(prefs.forced(view, max));
    prefs.shift(view, view.column_named("state").expect("column"), 2);
    let order = names(view, &prefs.ordered(view));
    assert_eq!(order[0], "NAME");
    assert_eq!(order[1..4], ["PROTO", "BIND", "ACC/s"]);
    assert_eq!(order[5], "STATE");
    prefs.shift(view, 0, 1);
    assert_eq!(names(view, &prefs.ordered(view))[0], "NAME");
}

#[test]
fn the_column_picker_hides_a_column_from_the_table_and_resets() {
    let mut app = running(View::Listeners);
    app.prepare(WIDE);
    let state = View::Listeners.column_named("state").expect("column");
    assert!(
        app.scene
            .table
            .columns
            .iter()
            .any(|column| column.index == state)
    );
    press(&mut app, "c");
    assert!(matches!(app.layers.last(), Some(Layer::Columns(0))));
    for _ in 0..state {
        app.key(Key::Down);
    }
    app.key(Key::Char(' '));
    app.key(Key::Escape);
    app.prepare(WIDE);
    assert!(
        app.scene
            .table
            .columns
            .iter()
            .all(|column| column.index != state)
    );
    assert_eq!(app.prefs.columns[&View::Listeners].off, ["state"]);
    press(&mut app, "c");
    press(&mut app, "r");
    app.key(Key::Escape);
    app.prepare(WIDE);
    assert!(app.prefs.columns.is_empty());
    assert!(
        app.scene
            .table
            .columns
            .iter()
            .any(|column| column.index == state)
    );
}

#[test]
fn the_column_set_follows_the_width_until_the_user_chooses() {
    let mut app = running(View::Listeners);
    app.prepare(WIDE);
    let wide = app.scene.table.columns.len();
    app.prepare(COMPACT);
    assert!(app.scene.table.columns.len() < wide);
    assert!(
        app.scene
            .table
            .columns
            .iter()
            .all(|column| column.x + column.width <= 80)
    );
}

#[test]
fn a_hidden_view_leaves_the_tabs_but_stays_in_the_picker() {
    let mut app = running(View::Upstreams);
    app.prepare(WIDE);
    press(&mut app, "V");
    assert!(matches!(app.layers.last(), Some(Layer::Views(1))));
    app.key(Key::Down);
    app.key(Key::Char(' '));
    assert_eq!(app.picker_len(), View::ALL.len());
    assert!(!app.visible_views().contains(&View::Clients));
    app.key(Key::Escape);
    app.prepare(WIDE);
    assert!(
        app.scene
            .tabs
            .slots
            .iter()
            .all(|tab| tab.view != View::Clients)
    );
    app.key(Key::Tab);
    assert_eq!(app.view, View::Conns);
    app.act(Action::GoTo(View::Clients));
    assert_eq!(app.view, View::Clients);
    press(&mut app, "V");
    app.key(Key::Char(' '));
    assert!(app.visible_views().contains(&View::Clients));
}

#[test]
fn views_reorder_and_the_last_visible_view_cannot_be_hidden() {
    let mut prefs = ViewPrefs::default();
    prefs.shift(View::Clients, -1);
    assert_eq!(prefs.visible()[1], View::Clients);
    prefs.shift(View::Clients, 1);
    assert_eq!(prefs, ViewPrefs::default());
    for view in View::ALL {
        prefs.toggle_hidden(view);
    }
    assert_eq!(prefs.visible(), [View::Generations]);
    assert!(prefs.is_hidden(View::Listeners));
}

#[test]
fn groups_open_and_close_one_node_or_all_and_remember_it_across_refreshes() {
    let mut app = running(View::Upstreams);
    app.prepare(WIDE);
    let keys = |app: &App| app.arranged.as_ref().expect("rows").keys();
    assert_eq!(
        keys(&app),
        [
            "api",
            "api/origin-c",
            "web",
            "web/origin-a",
            "web/origin-bb"
        ]
    );
    app.select_at(2);
    press(&mut app, " ");
    app.prepare(WIDE);
    assert_eq!(keys(&app), ["api", "api/origin-c", "web"]);
    refresh(&mut app);
    app.prepare(WIDE);
    assert_eq!(keys(&app), ["api", "api/origin-c", "web"]);
    assert_eq!(app.selected_key(), Some("web"));
    app.key(Key::Right);
    app.prepare(WIDE);
    assert_eq!(keys(&app).len(), 5);
    press(&mut app, "-");
    app.prepare(WIDE);
    assert_eq!(keys(&app), ["api", "web"]);
    press(&mut app, "+");
    app.prepare(WIDE);
    assert_eq!(keys(&app).len(), 5);
}

#[test]
fn left_goes_to_the_parent_and_enter_toggles_a_group() {
    let mut app = running(View::Upstreams);
    app.prepare(WIDE);
    app.select_at(3);
    assert_eq!(app.selected_key(), Some("web/origin-a"));
    app.key(Key::Left);
    assert_eq!(app.selected_key(), Some("web"));
    app.key(Key::Left);
    app.prepare(WIDE);
    assert_eq!(app.arranged.as_ref().expect("rows").len(), 3);
    app.key(Key::Enter);
    app.prepare(WIDE);
    assert_eq!(app.arranged.as_ref().expect("rows").len(), 5);
}

#[test]
fn clicking_the_marker_toggles_the_group_without_opening_the_drawer() {
    let mut app = running(View::Upstreams);
    app.prepare(WIDE);
    let marker = app.scene.table.rows[2].marker.expect("a marker");
    click(&mut app, marker);
    app.prepare(WIDE);
    assert_eq!(app.arranged.as_ref().expect("rows").len(), 3);
    assert!(app.layers.is_empty());
}

#[test]
fn a_filter_shows_matching_children_under_their_collapsed_parents() {
    let mut app = running(View::Upstreams);
    app.prepare(WIDE);
    press(&mut app, "-");
    app.set_filter("origin-bb");
    app.prepare(WIDE);
    assert_eq!(
        app.arranged.as_ref().expect("rows").keys(),
        ["web", "web/origin-bb"]
    );
}
