use std::time::Duration;

use super::fixture::{Load, sample, snapshot};
use crate::cli::top::{
    app::{App, Effect, Mode},
    keys::Key,
    rows::View,
};

fn app() -> App {
    let view = View::Listeners;
    App::new(
        view,
        view.default_sort(),
        String::new(),
        Duration::from_secs(1),
    )
}

fn load(requests: u64) -> Load {
    Load {
        requests,
        errors: 0,
        slow: 0,
    }
}

fn press(app: &mut App, keys: &str) {
    for character in keys.chars() {
        app.key(Key::Char(character));
    }
}

#[test]
fn tab_and_digits_switch_views_and_ask_the_sampler_for_requests_only_in_that_view() {
    let mut app = app();
    assert_eq!(app.key(Key::Tab), Effect::Retarget);
    assert_eq!(app.view, View::Pools);
    assert_eq!(app.key(Key::BackTab), Effect::Retarget);
    assert_eq!(app.view, View::Listeners);
    press(&mut app, "3");
    assert_eq!((app.view, app.control().requests), (View::Servers, false));
    press(&mut app, "4");
    assert_eq!((app.view, app.control().requests), (View::Requests, true));
    assert_eq!(app.sort().column, app.view.column_named("AGE").unwrap());
}

#[test]
fn sorting_moves_across_columns_per_view_and_reverses() {
    let mut app = app();
    let reqs = app.view.column_named("REQ/s").unwrap();
    press(&mut app, ">>>>>>");
    assert_eq!((app.sort().column, app.sort().descending), (reqs, true));
    press(&mut app, "r");
    assert!(!app.sort().descending);
    press(&mut app, "<");
    assert_eq!(app.sort().column, reqs - 1);
    press(&mut app, "2");
    assert_eq!(app.sort().column, 0);
    press(&mut app, "1");
    assert_eq!(app.sort().column, reqs - 1);
    press(&mut app, "<<<<<<");
    assert_eq!(app.sort().column, app.view.columns().len() - 1);
}

#[test]
fn the_filter_is_typed_kept_with_enter_and_cleared_with_escape() {
    let mut app = app();
    press(&mut app, "/web");
    assert_eq!(app.mode, Mode::Filter("web".into()));
    app.key(Key::Backspace);
    app.key(Key::Enter);
    assert_eq!((app.filter.as_str(), &app.mode), ("we", &Mode::Browse));
    press(&mut app, "/b");
    assert_eq!(app.mode, Mode::Filter("web".into()));
    assert_eq!(app.key(Key::Escape), Effect::Idle);
    assert_eq!((app.filter.as_str(), &app.mode), ("", &Mode::Browse));
}

#[test]
fn quitting_works_from_browse_and_from_the_filter_prompt() {
    assert_eq!(app().key(Key::Char('q')), Effect::Quit);
    assert_eq!(app().key(Key::Interrupt), Effect::Quit);
    let mut typing = app();
    press(&mut typing, "/");
    assert_eq!(typing.key(Key::Interrupt), Effect::Quit);
    let mut typing = app();
    press(&mut typing, "/q");
    assert_eq!(typing.mode, Mode::Filter("q".into()));
}

#[test]
fn pausing_freezes_the_display_until_resumed() {
    let mut app = app();
    app.sample(Ok(sample(snapshot(10_000, 100_000, &load(100)))));
    assert_eq!(app.key(Key::Char('p')), Effect::Retarget);
    assert!(app.control().paused);
    app.sample(Ok(sample(snapshot(12_000, 102_000, &load(200)))));
    assert_eq!(
        app.current.as_ref().unwrap().snapshot.sampled_at_unix_ms,
        10_000
    );
    press(&mut app, " ");
    assert!(!app.control().paused);
    app.sample(Ok(sample(snapshot(12_000, 102_000, &load(200)))));
    assert_eq!(
        app.current.as_ref().unwrap().snapshot.sampled_at_unix_ms,
        12_000
    );
    assert_eq!(app.previous.as_ref().unwrap().sampled_at_unix_ms, 10_000);
}

#[test]
fn samples_closer_than_a_second_keep_the_older_baseline() {
    let mut app = app();
    app.sample(Ok(sample(snapshot(10_000, 100_000, &load(100)))));
    app.sample(Ok(sample(snapshot(10_040, 100_040, &load(101)))));
    assert!(app.previous.is_none());
    app.sample(Ok(sample(snapshot(11_100, 101_100, &load(150)))));
    assert_eq!(app.previous.as_ref().unwrap().sampled_at_unix_ms, 10_040);
}

#[test]
fn a_failed_sample_shows_the_error_and_keeps_the_last_data() {
    let mut app = app();
    app.sample(Ok(sample(snapshot(10_000, 100_000, &load(100)))));
    app.sample(Err("endpoint connection failed".into()));
    assert_eq!(app.error.as_deref(), Some("endpoint connection failed"));
    assert!(app.table().is_some());
    app.sample(Ok(sample(snapshot(12_000, 102_000, &load(200)))));
    assert!(app.error.is_none());
}

#[test]
fn the_selection_stays_inside_the_rows() {
    let mut app = app();
    app.key(Key::End);
    app.clamp(3);
    assert_eq!(app.selected, 2);
    app.key(Key::Home);
    app.key(Key::Up);
    assert_eq!(app.selected, 0);
    app.clamp(0);
    assert_eq!(app.selected, 0);
}
