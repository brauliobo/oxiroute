use ratatui::layout::Rect;

use super::harness::{app, line_with, running, running_full, screen};
use crate::cli::top::{
    app::{App, Key},
    prefs::{Panel, PanelState, Prefs},
    theme::Capabilities,
    views::View,
};

const SIZES: [(u16, u16); 3] = [(120, 30), (100, 24), (80, 24)];

fn press(app: &mut App, keys: &str, size: (u16, u16)) {
    app.prepare(Rect::new(0, 0, size.0, size.1));
    for key in keys.chars() {
        app.key(Key::Char(key));
    }
}

fn header(screen: &str) -> &str {
    screen
        .lines()
        .find(|line| line.contains("NAME") && !line.contains("sort NAME"))
        .unwrap_or_else(|| panic!("no table header:\n{screen}"))
}

fn has_all(line: &str, words: &[&str]) -> bool {
    words.iter().all(|word| line.contains(word))
}

fn has_none(line: &str, words: &[&str]) -> bool {
    words.iter().all(|word| !line.contains(word))
}

#[test]
fn every_size_shows_the_identity_tabs_table_and_footer() {
    for (width, height) in SIZES {
        let screen = screen(&mut running(View::Listeners), width, height);
        let lines = screen.lines().collect::<Vec<_>>();
        assert!(
            lines[0].starts_with(" oxiroute 7.1.0  up 1m42s  generation #4 accepting"),
            "{width}: {}",
            lines[0]
        );
        assert!(lines[0].contains("2s rates"), "{}", lines[0]);
        assert!(line_with(&screen, "1 Listeners 1").contains("2 Upstreams 2"));
        assert!(line_with(&screen, "edge").contains("1.0k"));
        assert!(line_with(&screen, "TOTAL").contains("3.8M"));
        assert!(lines[lines.len() - 1].starts_with("F1 Help"));
        assert!(
            lines
                .iter()
                .all(|line| line.chars().count() <= usize::from(width))
        );
    }
}

#[test]
fn the_listener_columns_drop_by_priority_as_the_terminal_narrows() {
    let wide = screen(&mut running(View::Listeners), 120, 30);
    assert!(has_all(
        header(&wide),
        &[
            "PROTO", "BIND", "STATE", "ACC/s", "REQ/s", "P50", "P99", "CACHE"
        ]
    ));
    assert!(!header(&wide).contains("MAX"));
    let standard = screen(&mut running(View::Listeners), 100, 24);
    assert!(has_all(
        header(&standard),
        &["BIND", "ACC/s", "REQ/s", "P99"]
    ));
    assert!(has_none(header(&standard), &["PROTO", "P50", "CACHE"]));
    let compact = screen(&mut running(View::Listeners), 80, 24);
    assert!(has_all(
        header(&compact),
        &["STATE", "CONN", "REQ/s", "IN/s", "OUT/s", "ERR/s", "P99"]
    ));
    assert!(has_none(
        header(&compact),
        &["PROTO", "BIND", "ACC/s", "P50", "CACHE"]
    ));
}

#[test]
fn numbers_are_never_truncated_at_any_size() {
    for (width, height) in SIZES {
        let screen = screen(&mut running(View::Listeners), width, height);
        let edge = line_with(&screen, "edge");
        assert!(
            has_all(edge, &["1.0k", "3.8M", "488K", "500ms", "5/4.1k"]),
            "{width}: {edge}"
        );
    }
}

#[test]
fn the_header_meters_become_a_strip_when_narrow() {
    let wide = screen(&mut running(View::Listeners), 120, 30);
    assert!(has_all(
        &wide,
        &["Requests/s", "peak 1.0k/s", "cpu", "mem 143M"]
    ));
    let compact = screen(&mut running(View::Listeners), 80, 24);
    assert!(!compact.contains("Requests/s"));
    assert!(line_with(&compact, "req/s").contains("in 3.8MB/s"));
}

#[test]
fn the_upstreams_are_a_tree_of_pools_and_servers() {
    let wide = screen(&mut running(View::Upstreams), 120, 30);
    assert!(line_with(&wide, "▾ api").contains("up"));
    assert!(line_with(&wide, "origin-bb").contains("down"));
    assert!(has_all(header(&wide), &["HEALTH", "ADDRESS", "P99"]));
    let compact = screen(&mut running(View::Upstreams), 80, 24);
    assert!(has_none(header(&compact), &["HEALTH", "ADDRESS"]));
    assert!(compact.contains("origin-a"));
}

#[test]
fn a_closed_group_hides_its_servers_but_keeps_its_totals() {
    let mut app = running(View::Upstreams);
    press(&mut app, "-", (120, 30));
    let screen = screen(&mut app, 120, 30);
    assert!(!screen.contains("origin-a"));
    assert!(line_with(&screen, "▸ api").contains("up"));
    assert!(screen.contains("TOTAL"));
}

#[test]
fn clients_show_rates_totals_and_the_pinned_other_and_total_rows() {
    let screen = screen(&mut running_full(View::Clients), 120, 30);
    assert!(has_all(
        line_with(&screen, "CLIENT"),
        &["NEW/s", "REQ/s", "IN tot", "ERR%"]
    ));
    assert!(has_all(
        line_with(&screen, "192.0.2.10"),
        &["900", "3.8M", "1.0%"]
    ));
    assert!(line_with(&screen, "other (1)").contains("500"));
    assert!(line_with(&screen, "TOTAL").contains("925"));
    assert!(line_with(&screen, "Clients (top 2 of 3)").contains("2s rates"));
    assert!(screen.contains("ipv6 grouped /64"));
}

#[test]
fn conns_requests_rtmp_and_generations_render_their_rows() {
    let conns = screen(&mut running_full(View::Conns), 120, 30);
    assert!(has_all(
        line_with(&conns, "192.0.2.10:51000"),
        &["edge", "h2", "web", "active"]
    ));
    let requests = screen(&mut running_full(View::Requests), 120, 30);
    assert!(has_all(
        line_with(&requests, "req-old"),
        &["GET", "/req-old", "streaming", "42s", "origin-a"]
    ));
    assert!(line_with(&requests, "overdue").contains("in flight 2"));
    let rtmp = screen(&mut running_full(View::Rtmp), 120, 30);
    assert!(line_with(&rtmp, "live/show").contains("stream"));
    assert!(line_with(&rtmp, "192.0.2.20").contains("publisher"));
    let generations = screen(&mut running_full(View::Generations), 120, 30);
    assert!(has_all(
        line_with(&generations, "rev-4"),
        &["active", "accepting", "rev-4"]
    ));
}

#[test]
fn a_collapsed_meters_panel_keeps_one_line_at_every_tier() {
    for size in [(120, 30), (80, 24)] {
        let mut app = running(View::Listeners);
        press(&mut app, "h", size);
        let screen = screen(&mut app, size.0, size.1);
        assert!(
            line_with(&screen, "▸ Meters").contains("(h expands, H hides)"),
            "{size:?}"
        );
        assert!(!screen.contains("peak 1.0k/s"));
        assert_eq!(app.prefs.panels.get(Panel::Meters), PanelState::Collapsed);
    }
}

#[test]
fn a_hidden_panel_disappears_and_the_footer_offers_it_back() {
    for size in [(120, 30), (80, 24)] {
        let mut app = running(View::Listeners);
        press(&mut app, "HS", size);
        let screen = screen(&mut app, size.0, size.1);
        assert!(!screen.contains("Requests/s") && !screen.contains("Listeners summary"));
        let footer = screen.lines().last().expect("footer");
        assert!(
            has_all(footer, &["hidden:", "H Meters", "S Summary"]),
            "{size:?}: {footer}"
        );
    }
}

#[test]
fn a_hidden_column_is_gone_and_the_rest_keep_their_places() {
    for size in [(120, 30), (80, 24)] {
        let mut app = running(View::Listeners);
        press(&mut app, "c", size);
        let req = View::Listeners.column_named("req").expect("column");
        for _ in 0..req {
            app.key(Key::Down);
        }
        app.key(Key::Char(' '));
        app.key(Key::Escape);
        let screen = screen(&mut app, size.0, size.1);
        assert!(!header(&screen).contains("REQ/s"), "{size:?}");
        assert!(has_all(header(&screen), &["NAME", "STATE", "IN/s", "P99"]));
        assert!(line_with(&screen, "edge").contains("3.8M"));
    }
}

#[test]
fn a_column_the_default_hides_can_be_shown_on_demand() {
    let mut app = running(View::Listeners);
    press(&mut app, "c", (80, 24));
    let max = View::Listeners.column_named("max").expect("column");
    for _ in 0..max {
        app.key(Key::Down);
    }
    app.key(Key::Char(' '));
    app.key(Key::Escape);
    let screen = screen(&mut app, 80, 24);
    assert!(header(&screen).contains("MAX"));
    assert!(line_with(&screen, "edge").contains("4.1k"));
}

#[test]
fn compact_density_is_a_plain_table_and_focus_hides_the_chrome() {
    let mut app = running(View::Listeners);
    press(&mut app, "d", (120, 30));
    let dense = screen(&mut app, 120, 30);
    assert!(!dense.contains('╭') && !dense.contains("summary") && !dense.contains("Requests/s"));
    assert!(has_all(header(&dense), &["NAME", "REQ/s"]));
    let mut app = running(View::Listeners);
    press(&mut app, "f", (80, 24));
    let focused = screen(&mut app, 80, 24);
    assert!(!focused.contains("oxiroute 7.1.0") && !focused.contains("Detail"));
    assert!(focused.contains("edge"));
}

#[test]
fn the_drawer_opens_beside_the_table_when_wide() {
    let mut app = running(View::Listeners);
    press(&mut app, "i", (120, 30));
    let screen = screen(&mut app, 120, 30);
    assert!(line_with(&screen, "Detail edge").contains("Listeners (1)"));
    assert!(has_all(
        &screen,
        &[
            "bind      socket:127.0.0.1:8080",
            "cache     9000 hits, 3000 misses"
        ]
    ));
}

#[test]
fn every_drawer_separates_each_label_from_its_value() {
    for view in View::ALL {
        let mut app = running_full(view);
        press(&mut app, "i", (120, 60));
        let screen = screen(&mut app, 120, 60);
        let lines = &app.scene.drawer.as_ref().expect("a drawer").lines;
        for (label, value) in lines {
            let edge = format!("│{label}");
            let row = screen
                .lines()
                .skip_while(|line| !line.contains("Detail "))
                .find(|line| line.contains(&edge))
                .unwrap_or_else(|| panic!("{view:?}: no drawer line holds {label}:\n{screen}"));
            let after = row.split_once(&edge).expect("the label").1;
            assert!(
                after.starts_with(' ') && !after.trim().is_empty() && !value.is_empty(),
                "{view:?}: {label:?} touches its value in {row:?}"
            );
        }
    }
}

#[test]
fn the_pool_drawer_sizes_its_label_column_to_the_longest_label() {
    let mut app = running_full(View::Upstreams);
    press(&mut app, "i", (120, 30));
    let screen = screen(&mut app, 120, 30);
    assert!(has_all(
        &screen,
        &["unavailable selections 0", "fail-open selections   0"]
    ));
}

#[test]
fn the_picker_and_help_overlays_list_what_can_be_changed() {
    let mut app = running(View::Listeners);
    press(&mut app, "c", (120, 30));
    let columns = screen(&mut app, 120, 30);
    assert!(has_all(
        &columns,
        &["Columns of Listeners", "[x] NAME", "[ ] MAX", "P4"]
    ));
    let mut app = running(View::Listeners);
    press(&mut app, "V", (80, 24));
    let views = screen(&mut app, 80, 24);
    assert!(has_all(
        &views,
        &["Views (hidden ones stay reachable here)", "[x] Generations"]
    ));
    let mut app = running(View::Listeners);
    press(&mut app, "?", (80, 24));
    let help = screen(&mut app, 80, 24);
    assert!(has_all(
        &help,
        &[
            "Keys (Up/Down scroll, Esc closes)",
            "choose, reorder and hide columns"
        ]
    ));
}

#[test]
fn a_terminal_below_sixty_by_twelve_says_so() {
    let narrow = screen(&mut running(View::Listeners), 59, 24);
    assert_eq!(narrow.trim(), "terminal too small: need 60x12, have 59x24");
    let short = screen(&mut running(View::Listeners), 80, 11);
    assert!(short.contains("need 60x12, have 80x11"));
}

#[test]
fn before_the_first_sample_the_dashboard_says_it_is_waiting() {
    let mut app = app(
        View::Listeners,
        Prefs::default(),
        Capabilities {
            depth: crate::cli::top::theme::Depth::None,
            unicode: true,
        },
    );
    let screen = screen(&mut app, 120, 30);
    assert!(screen.contains("waiting for the first snapshot"));
}

#[test]
fn a_failed_poll_is_shown_without_losing_the_last_data() {
    let mut app = running(View::Listeners);
    app.sample(Err("the daemon stopped answering".into()));
    let screen = screen(&mut app, 120, 30);
    assert!(screen.contains("the daemon stopped answering"));
    assert!(screen.contains("edge"));
}

#[test]
fn the_same_row_renders_in_bits_when_asked() {
    let mut app = running(View::Listeners);
    press(&mut app, "u", (120, 30));
    let screen = screen(&mut app, 120, 30);
    assert!(line_with(&screen, "edge").contains("32Mb"));
}
