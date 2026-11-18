use std::time::Duration;

use ratatui::{Terminal, backend::TestBackend};

use super::fixture::{Load, sample, snapshot, with_requests};
use crate::cli::top::{
    app::App,
    draw,
    keys::Key,
    once,
    rows::{Sort, View},
};

fn load(requests: u64, errors: u64, slow: u64) -> Load {
    Load {
        requests,
        errors,
        slow,
    }
}

fn running(view: View) -> App {
    let mut app = App::new(
        view,
        view.default_sort(),
        String::new(),
        Duration::from_secs(1),
    );
    app.sample(Ok(sample(snapshot(10_000, 100_000, &load(1_000, 0, 0)))));
    app.sample(Ok(sample(snapshot(12_000, 102_000, &load(3_000, 40, 60)))));
    app
}

fn screen(app: &App, width: u16, height: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
    let table = app.table();
    terminal
        .draw(|frame| draw::frame(frame, app, table.as_ref()))
        .expect("draw");
    let buffer = terminal.backend().buffer();
    buffer
        .content()
        .chunks(usize::from(width))
        .map(|row| {
            row.iter()
                .map(ratatui::buffer::Cell::symbol)
                .collect::<String>()
                .trim_end()
                .to_owned()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn the_listeners_view_shows_the_summary_tabs_and_one_row_per_listener() {
    let screen = screen(&running(View::Listeners), 150, 14);
    let lines: Vec<_> = screen.lines().collect();
    assert!(lines[0].starts_with(
        "oxiroute 7.1.0  up 1m42s  generation #4 accepting  workers dynamic 2/4 active"
    ));
    assert!(lines[1].contains("cpu 12%  rss 143M"), "{}", lines[1]);
    assert!(
        lines[2].contains("accepted 1.0k/s") && lines[2].contains("conn 7/4.1k"),
        "{}",
        lines[2]
    );
    assert!(
        lines[3].contains("1:Listeners")
            && lines[3].contains("sort NAME asc")
            && lines[3].contains("window")
    );
    assert!(lines[4].starts_with("NAME^"), "{}", lines[4]);
    assert!(
        lines[5].starts_with("edge") && lines[5].contains("socket:127.0.0.1:8080"),
        "{}",
        lines[5]
    );
    assert!(
        lines[5].contains("up") && lines[5].contains("5/4.1k") && lines[5].contains("1.0k"),
        "{}",
        lines[5]
    );
    assert!(lines.last().unwrap().contains("q quit"));
}

#[test]
fn the_servers_view_lists_every_server_with_status_classes_and_latency() {
    let screen = screen(&running(View::Servers), 170, 14);
    assert!(screen.contains("POOL"), "{screen}");
    let origin_a = screen
        .lines()
        .find(|line| line.contains("origin-a"))
        .expect("origin-a row");
    for part in ["web", "up", "2/50", "3.0k", "40", "500ms"] {
        assert!(origin_a.contains(part), "{part} in {origin_a}");
    }
    assert!(
        screen
            .lines()
            .any(|line| line.contains("origin-bb") && line.contains("down"))
    );
    assert!(
        screen
            .lines()
            .any(|line| line.contains("origin-c") && line.contains("drain"))
    );
}

#[test]
fn the_filter_and_pause_state_show_in_the_tab_line_and_the_rows() {
    let mut app = running(View::Servers);
    for key in "/origin-a".chars() {
        app.key(Key::Char(key));
    }
    app.key(Key::Enter);
    app.key(Key::Char('p'));
    let screen = screen(&app, 170, 12);
    assert!(
        screen.contains("filter \"origin-a\"") && screen.contains("PAUSED"),
        "{screen}"
    );
    assert!(
        screen.contains("origin-a")
            && !screen.contains("origin-bb")
            && !screen.contains("origin-c")
    );
}

#[test]
fn an_error_after_data_stays_on_screen_below_the_last_snapshot() {
    let mut app = running(View::Pools);
    app.sample(Err("endpoint connection failed".into()));
    let screen = screen(&app, 140, 12);
    assert!(
        screen.contains("error: endpoint connection failed"),
        "{screen}"
    );
    assert!(screen.lines().any(|line| line.starts_with("web")));
}

#[test]
fn the_requests_view_lists_in_flight_requests_oldest_first_with_the_total() {
    let mut app = App::new(
        View::Requests,
        View::Requests.default_sort(),
        String::new(),
        Duration::from_secs(1),
    );
    app.sample(Ok(with_requests(snapshot(
        12_000,
        102_000,
        &load(3_000, 0, 0),
    ))));
    let screen = screen(&app, 170, 12);
    assert!(screen.contains("3 in flight, first 2 shown"), "{screen}");
    let lines: Vec<_> = screen.lines().collect();
    assert!(
        lines[4].starts_with("ID") && lines[4].contains("AGE") && lines[4].contains("PHASE"),
        "{}",
        lines[4]
    );
    for part in [
        "req-old",
        "POST",
        "app.example.com",
        "/v1/render",
        "origin-a",
        "streaming",
        "42s",
        "2",
    ] {
        assert!(lines[5].contains(part), "{part} in {}", lines[5]);
    }
    for part in ["req-queued", "GET", "queued", "1.5s", "1.4s"] {
        assert!(lines[6].contains(part), "{part} in {}", lines[6]);
    }
}

#[test]
fn a_small_terminal_still_draws_without_panicking() {
    for (width, height) in [(1, 1), (20, 5), (40, 8)] {
        screen(&running(View::Servers), width, height);
    }
}

#[test]
fn once_prints_the_summary_and_the_three_snapshot_views_as_aligned_text() {
    let sample = sample(snapshot(12_000, 102_000, &load(3_000, 40, 60)));
    let views = [View::Listeners, View::Pools, View::Servers];
    let sorts = views.map(View::default_sort);
    let text = once::render(&sample, &views, &sorts, "");
    assert!(text.starts_with("oxiroute 7.1.0"));
    for title in ["\nListeners\n", "\nPools\n", "\nServers\n"] {
        assert!(text.contains(title), "{title}");
    }
    let servers = text.split("\nServers\n").nth(1).unwrap();
    let header = servers.lines().next().unwrap();
    let row = servers
        .lines()
        .find(|line| line.starts_with("web") && line.contains("origin-a"))
        .unwrap();
    assert!(header.starts_with("POOL SERVER"), "{header}");
    assert_eq!(header.find("STATUS"), row.find("up"));
    assert!(text.ends_with('\n') && !text.contains('\t'));
    let filtered = once::render(
        &sample,
        &[View::Servers],
        &[Sort {
            column: 1,
            descending: false,
        }],
        "origin-c",
    );
    assert!(filtered.contains("origin-c") && !filtered.contains("origin-a"));
}
