use ratatui::layout::Rect;

use super::{
    fixture::{load, snapshot, with_requests},
    harness::{PLAIN, app},
};
use crate::cli::top::{
    app::{Action, App, Effect, Key, Layer},
    prefs::Prefs,
    views::View,
};

const AREA: Rect = Rect::new(0, 0, 120, 30);

fn feed(app: &mut App, at: u64, requests: &[(&str, u64)]) {
    let snapshot = snapshot(at, at * 10, &load(at, 0, 0));
    app.sample(Ok(with_requests(snapshot, requests)));
    app.prepare(AREA);
}

fn requests() -> App {
    let mut app = app(View::Requests, Prefs::default(), PLAIN);
    feed(
        &mut app,
        10_000,
        &[("a", 1_000), ("b", 2_000), ("c", 3_000)],
    );
    app
}

fn keys(app: &App) -> Vec<String> {
    app.arranged.as_ref().expect("rows").keys()
}

#[test]
fn the_selection_follows_its_row_when_the_order_changes() {
    let mut app = requests();
    assert_eq!(keys(&app), ["c", "b", "a"]);
    app.select_at(1);
    assert_eq!(app.selected_key(), Some("b"));
    feed(
        &mut app,
        11_000,
        &[("d", 9_000), ("a", 1_500), ("b", 2_500), ("c", 3_500)],
    );
    assert_eq!(keys(&app), ["d", "c", "b", "a"]);
    assert_eq!(app.selected_key(), Some("b"));
    assert_eq!(app.selection.position, 2);
    assert!(!app.selection.gone);
}

#[test]
fn a_vanished_row_keeps_its_place_and_is_marked_gone() {
    let mut app = requests();
    app.select_at(1);
    feed(&mut app, 11_000, &[("a", 1_500), ("c", 3_500)]);
    assert_eq!(app.selected_key(), Some("b"));
    assert!(app.selection.gone);
    assert_eq!(app.selection.position, 1);
    feed(
        &mut app,
        12_000,
        &[("a", 1_500), ("b", 2_500), ("c", 3_500)],
    );
    assert!(!app.selection.gone);
    assert_eq!(keys(&app)[app.selection.position], "b");
}

#[test]
fn freezing_the_order_keeps_rows_in_place_while_their_values_change() {
    let mut app = requests();
    app.key(Key::Char('o'));
    app.prepare(AREA);
    feed(
        &mut app,
        11_000,
        &[("a", 9_000), ("b", 2_500), ("c", 3_500)],
    );
    assert_eq!(keys(&app), ["c", "b", "a"]);
    app.key(Key::Char('o'));
    app.prepare(AREA);
    assert_eq!(keys(&app), ["a", "c", "b"]);
}

#[test]
fn vim_keys_and_arrows_move_the_selection() {
    let mut app = requests();
    app.key(Key::Char('j'));
    assert_eq!(app.selected_key(), Some("b"));
    app.key(Key::Down);
    assert_eq!(app.selected_key(), Some("a"));
    app.key(Key::Down);
    assert_eq!(app.selected_key(), Some("a"));
    app.key(Key::Char('k'));
    app.key(Key::Up);
    assert_eq!(app.selected_key(), Some("c"));
    app.key(Key::End);
    assert_eq!(app.selected_key(), Some("a"));
    app.key(Key::Char('g'));
    assert_eq!(app.selected_key(), Some("c"));
}

#[test]
fn escape_closes_the_top_layer_and_q_quits() {
    let mut app = requests();
    app.key(Key::Char('?'));
    app.act(Action::OpenFilter);
    assert!(matches!(
        app.layers.as_slice(),
        [Layer::Help(_), Layer::Filter(_)]
    ));
    assert_eq!(app.key(Key::Escape), Effect::Idle);
    assert!(matches!(app.layers.as_slice(), [Layer::Help(_)]));
    assert_eq!(app.key(Key::Escape), Effect::Idle);
    assert!(app.layers.is_empty());
    assert_eq!(app.key(Key::Char('q')), Effect::Quit);
}

#[test]
fn the_filter_is_incremental_and_escape_clears_it() {
    let mut app = requests();
    app.key(Key::Char('/'));
    for key in "id:b".chars() {
        app.key(Key::Char(key));
    }
    app.prepare(AREA);
    assert_eq!(keys(&app), ["b"]);
    app.key(Key::Enter);
    assert!(app.layers.is_empty());
    assert_eq!(keys(&app), ["b"]);
    app.key(Key::Escape);
    app.prepare(AREA);
    assert_eq!(keys(&app), ["c", "b", "a"]);
    app.key(Key::Char('/'));
    for key in "!id:a".chars() {
        app.key(Key::Char(key));
    }
    app.prepare(AREA);
    assert_eq!(keys(&app), ["c", "b"]);
    app.key(Key::Escape);
    app.prepare(AREA);
    assert_eq!(keys(&app), ["c", "b", "a"]);
}

#[test]
fn drilling_down_is_undone_one_step_at_a_time() {
    let mut app = app(View::Listeners, Prefs::default(), PLAIN);
    app.sample(Ok(super::harness::first_sample()));
    app.prepare(AREA);
    assert_eq!(app.selected_key(), Some("edge"));
    app.act(Action::Drill);
    assert_eq!(app.view, View::Conns);
    assert_eq!(app.filter.source(), "listener:edge");
    app.key(Key::Escape);
    assert_eq!(app.view, View::Listeners);
    assert_eq!(app.selected_key(), Some("edge"));
    assert!(app.filter.is_empty());
}
