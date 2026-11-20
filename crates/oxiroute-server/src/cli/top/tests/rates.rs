use serde_json::json;

use super::{
    fixture::{load, sample, snapshot, snapshot_document},
    harness::{PLAIN, app, first_sample, running, second_sample},
};
use crate::cli::top::{
    app::Key,
    history::{Clock, History, Pushed, Window},
    prefs::Prefs,
    views::View,
};

fn pushed(history: &mut History, at: u64, uptime: u64) -> Pushed {
    history.push(sample(snapshot(at, uptime, &load(at, 0, 0))))
}

#[test]
fn a_windowed_clock_turns_counter_deltas_into_rates() {
    let clock = Clock::delta(12_000, 10_000);
    assert_eq!(clock.rate(3_000, Some(1_000), None), Some(1_000.0));
    assert_eq!(clock.change(3_000, Some(1_000)), Some(2_000));
    assert!(clock.windowed());
}

#[test]
fn a_filling_clock_has_no_rates_yet() {
    let clock = Clock::filling();
    assert_eq!(clock.rate(3_000, Some(1_000), None), None);
    assert_eq!(clock.change(3_000, Some(1_000)), None);
    assert!(!clock.windowed());
    assert!(!Clock::delta(10_000, 10_000).windowed());
}

#[test]
fn a_counter_that_went_backwards_has_no_rate() {
    let clock = Clock::delta(12_000, 10_000);
    assert_eq!(clock.rate(5, Some(10), None), None);
    assert_eq!(clock.change(5, Some(10)), None);
}

#[test]
fn a_key_born_inside_the_window_is_rated_over_its_own_age() {
    let clock = Clock::delta(12_000, 10_000);
    assert_eq!(clock.rate(3_000, None, Some(11_000)), Some(3_000.0));
    assert_eq!(clock.rate(3_000, None, Some(9_000)), None);
    assert_eq!(clock.rate(3_000, None, None), None);
}

#[test]
fn the_life_window_divides_by_uptime_or_by_the_key_age() {
    let clock = Clock::life(100_000, 50_000);
    assert_eq!(clock.rate(500, None, None), Some(10.0));
    assert_eq!(clock.rate(500, None, Some(90_000)), Some(50.0));
    assert_eq!(clock.change(500, None), Some(500));
}

#[test]
fn an_older_or_equal_timestamp_is_stale() {
    let mut history = History::default();
    assert_eq!(pushed(&mut history, 10_000, 1_000), Pushed::Added);
    assert_eq!(pushed(&mut history, 10_000, 1_000), Pushed::Stale);
    assert_eq!(pushed(&mut history, 9_000, 1_000), Pushed::Stale);
    assert_eq!(pushed(&mut history, 11_000, 2_000), Pushed::Added);
}

#[test]
fn a_shorter_uptime_clears_the_history() {
    let mut history = History::default();
    pushed(&mut history, 10_000, 100_000);
    pushed(&mut history, 11_000, 101_000);
    assert_eq!(pushed(&mut history, 12_000, 500), Pushed::Reset);
    assert!(history.baseline(Window::Short, |_| true).is_none());
    assert!(history.series().last().is_none());
}

#[test]
fn the_meter_series_follows_each_sample_and_is_bounded() {
    let mut history = History::default();
    for step in 0..100 {
        pushed(&mut history, 10_000 + step * 1_000, 100_000 + step * 1_000);
    }
    assert_eq!(history.series().bars(|point| point.requests).len(), 64);
    assert!((history.series().peak(|point| point.requests) - 1_000.0).abs() < 1e-9);
}

#[test]
fn a_new_generation_clears_the_history() {
    let mut history = History::default();
    pushed(&mut history, 10_000, 100_000);
    let mut document = snapshot_document(11_000, 101_000, &load(1, 0, 0));
    document["generation"]["number"] = json!(5);
    let next = sample(serde_json::from_value(document).expect("snapshot"));
    assert_eq!(history.push(next), Pushed::Reset);
    assert!(history.baseline(Window::Short, |_| true).is_none());
}

#[test]
fn the_baseline_is_the_latest_sample_at_or_before_the_window() {
    let mut history = History::default();
    for step in 0..4 {
        pushed(&mut history, 10_000 + step * 1_000, 100_000 + step * 1_000);
    }
    let short = history.baseline(Window::Short, |_| true).expect("baseline");
    assert_eq!(short.sample.snapshot.sampled_at_unix_ms, 11_000);
    assert!(short.complete);
    let medium = history
        .baseline(Window::Medium, |_| true)
        .expect("baseline");
    assert_eq!(medium.sample.snapshot.sampled_at_unix_ms, 10_000);
    assert!(!medium.complete);
    assert!(history.baseline(Window::Life, |_| true).is_none());
}

#[test]
fn the_history_keeps_about_a_minute_of_samples() {
    let mut history = History::default();
    for step in 0..120 {
        pushed(&mut history, 10_000 + step * 1_000, 100_000 + step * 1_000);
    }
    let latest = 10_000 + 119 * 1_000;
    let long = history.baseline(Window::Long, |_| true).expect("baseline");
    assert_eq!(long.sample.snapshot.sampled_at_unix_ms, latest - 60_000);
    assert!(long.complete);
}

#[test]
fn the_listener_rate_is_the_counter_delta_over_the_window() {
    let mut app = running(View::Listeners);
    app.prepare(ratatui::layout::Rect::new(0, 0, 120, 30));
    let arranged = app.arranged.as_ref().expect("rows");
    let column = View::Listeners.column_named("req").expect("column");
    let rate = arranged.row(0).expect("row").cells[column]
        .number_value()
        .expect("a rate");
    assert!((rate - 1_020.0).abs() < 0.5, "{rate}");
}

#[test]
fn a_window_longer_than_the_history_says_it_is_still_filling() {
    let mut app = running(View::Listeners);
    app.key(Key::Char('w'));
    app.prepare(ratatui::layout::Rect::new(0, 0, 120, 30));
    assert!(app.scene.table.title.contains("10s rates filling"));
    app.key(Key::Char('w'));
    app.key(Key::Char('w'));
    app.key(Key::Char('w'));
    app.prepare(ratatui::layout::Rect::new(0, 0, 120, 30));
    assert!(app.scene.table.title.contains("2s rates"));
    assert!(!app.scene.table.title.contains("filling"));
}

#[test]
fn a_restart_tells_the_user_and_starts_the_rates_over() {
    let mut app = app(View::Listeners, Prefs::default(), PLAIN);
    app.sample(Ok(first_sample()));
    app.sample(Ok(second_sample()));
    assert_eq!(app.message, None);
    app.sample(Ok(sample(snapshot(14_000, 1_000, &load(5, 0, 0)))));
    assert!(
        app.message
            .as_deref()
            .is_some_and(|message| message.contains("rate history cleared"))
    );
    assert!(app.sampling().expect("sampling").before.is_none());
}
