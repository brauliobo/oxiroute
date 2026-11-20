use ratatui::layout::Rect;

use super::harness::{draw, running, text};
use crate::cli::top::{
    app::{Action, Key},
    layout::{Candidate, DrawerMode, Inputs, Layout, Meters, Summary, Tier, fit},
    prefs::{Density, Panel, PanelState},
    scene::Hit,
    views::View,
};

fn inputs(width: u16, height: u16) -> Inputs {
    Inputs {
        area: Rect::new(0, 0, width, height),
        density: Density::Normal,
        focus: false,
        meters: PanelState::Expanded,
        summary: PanelState::Expanded,
        drawer: DrawerMode::Collapsed,
    }
}

fn candidate(index: usize, width: u16, priority: u8, forced: bool) -> Candidate {
    Candidate {
        index,
        width,
        priority,
        forced,
    }
}

fn kept(candidates: Vec<Candidate>, available: u16) -> Vec<usize> {
    fit(candidates, available)
        .iter()
        .map(|candidate| candidate.index)
        .collect()
}

#[test]
fn the_tier_follows_the_terminal_width() {
    let tiers = [59, 79, 80, 99, 100, 119, 120, 200].map(Tier::of);
    assert_eq!(
        tiers,
        [
            Tier::Minimal,
            Tier::Minimal,
            Tier::Compact,
            Tier::Compact,
            Tier::Standard,
            Tier::Standard,
            Tier::Wide,
            Tier::Wide
        ]
    );
}

#[test]
fn columns_are_dropped_lowest_priority_first_and_never_truncated() {
    let columns = || {
        vec![
            candidate(0, 20, 1, false),
            candidate(1, 8, 1, false),
            candidate(2, 8, 2, false),
            candidate(3, 8, 3, false),
            candidate(4, 8, 4, false),
        ]
    };
    assert_eq!(kept(columns(), 60), [0, 1, 2, 3, 4]);
    assert_eq!(kept(columns(), 47), [0, 1, 2, 3]);
    assert_eq!(kept(columns(), 38), [0, 1, 2]);
    assert_eq!(kept(columns(), 29), [0, 1]);
    for kept in fit(columns(), 38).iter().skip(1) {
        assert_eq!(kept.width, 8);
    }
}

#[test]
fn a_column_the_user_chose_outlives_the_defaults() {
    let columns = vec![
        candidate(0, 20, 1, false),
        candidate(1, 8, 1, false),
        candidate(2, 8, 2, false),
        candidate(3, 8, 4, true),
    ];
    assert_eq!(kept(columns, 38), [0, 1, 3]);
}

#[test]
fn the_label_column_shrinks_only_when_nothing_else_can_go() {
    let label = |available| fit(vec![candidate(0, 30, 1, false)], available)[0].width;
    assert_eq!(label(15), 15);
    assert_eq!(label(5), 8);
    assert_eq!(label(40), 30);
}

#[test]
fn the_screen_is_split_without_overlaps_at_each_tier() {
    for (width, height) in [(120, 30), (100, 24), (80, 24), (80, 14)] {
        let layout = Layout::compute(&inputs(width, height));
        assert!(!layout.too_small);
        assert_eq!(layout.keys.y, height - 1);
        assert_eq!(layout.message.y, height - 2);
        let drawer = layout.drawer.expect("a drawer title").rect();
        assert_eq!(layout.table.y + layout.table.height, drawer.y);
        assert_eq!(drawer.y + drawer.height, layout.message.y);
        assert!(layout.table.height >= 6, "{width}x{height}");
    }
}

#[test]
fn the_header_panels_shrink_with_the_width_and_the_height() {
    let wide = Layout::compute(&inputs(120, 30));
    assert!(matches!(wide.meters, Some(Meters::Boxes(rect)) if rect.height == 5));
    assert!(matches!(wide.summary, Some(Summary::Box(rect)) if rect.height == 3));
    let compact = Layout::compute(&inputs(80, 24));
    assert!(matches!(compact.meters, Some(Meters::Strip(_))));
    let short = Layout::compute(&inputs(80, 14));
    assert!(short.summary.is_none());
    assert!(short.meters.is_some());
}

#[test]
fn a_terminal_below_sixty_by_twelve_is_too_small() {
    assert!(Layout::compute(&inputs(59, 24)).too_small);
    assert!(Layout::compute(&inputs(80, 11)).too_small);
    assert!(!Layout::compute(&inputs(60, 12)).too_small);
}

#[test]
fn collapsed_hidden_and_compact_panels_change_the_rows_they_take() {
    let mut collapsed = inputs(120, 30);
    collapsed.meters = PanelState::Collapsed;
    collapsed.summary = PanelState::Collapsed;
    let layout = Layout::compute(&collapsed);
    assert!(matches!(layout.meters, Some(Meters::Title(rect)) if rect.height == 1));
    assert!(matches!(layout.summary, Some(Summary::Title(rect)) if rect.height == 1));
    let mut hidden = inputs(120, 30);
    hidden.meters = PanelState::Hidden;
    hidden.summary = PanelState::Hidden;
    let layout = Layout::compute(&hidden);
    assert!(layout.meters.is_none() && layout.summary.is_none());
    let mut dense = inputs(120, 30);
    dense.density = Density::Compact;
    let layout = Layout::compute(&dense);
    assert!(layout.meters.is_none() && layout.summary.is_none() && !layout.bordered);
}

#[test]
fn a_click_lands_on_what_is_drawn_there() {
    let mut app = running(View::Listeners);
    let buffer = draw(&mut app, 120, 30);
    let screen = text(&buffer);
    let lines = screen.lines().collect::<Vec<_>>();
    let scene = &app.scene;

    let tab = &scene.tabs.slots[1];
    assert_eq!(scene.hit(tab.rect.x, tab.rect.y), Hit::Tab(tab.view));
    assert!(lines[usize::from(tab.rect.y)].contains("Upstreams"));

    let header = &scene.table.columns[3];
    assert_eq!(
        scene.hit(header.x, scene.table.inner.y),
        Hit::Header(header.index)
    );
    assert!(lines[usize::from(scene.table.inner.y)].contains(&header.title));

    let row = &scene.table.rows[0];
    assert_eq!(scene.hit(scene.table.inner.x + 4, row.y), Hit::Row(0));
    assert!(lines[usize::from(row.y)].contains(row.cells[0].0.trim()));

    let chip = &scene.chips[0];
    assert_eq!(scene.hit(chip.rect.x, chip.rect.y), Hit::Chip(chip.action));
    assert!(lines[usize::from(chip.rect.y)].contains(&chip.name));

    let (title, panel) = scene.titles[0];
    assert_eq!(scene.hit(title.x + 1, title.y), Hit::Title(panel));
    assert_eq!(panel, Panel::Meters);
}

#[test]
fn an_open_layer_takes_the_clicks_and_outside_clicks_close_it() {
    let mut app = running(View::Listeners);
    app.key(Key::Char('c'));
    draw(&mut app, 120, 30);
    let overlay = app.scene.overlay.as_ref().expect("an overlay");
    let item = overlay.items[2].rect;
    let rect = overlay.rect;
    assert_eq!(app.scene.hit(item.x, item.y), Hit::Item(2));
    assert_eq!(
        app.scene.hit(rect.x + rect.width, rect.y + rect.height),
        Hit::Outside
    );
    assert_eq!(app.scene.hit(0, 0), Hit::Outside);
    app.act(Action::Pop);
    draw(&mut app, 120, 30);
    assert!(app.scene.overlay.is_none());
}

#[test]
fn rows_scrolled_out_of_the_viewport_are_never_formatted() {
    let mut app = running(View::Upstreams);
    draw(&mut app, 120, 14);
    let scene = &app.scene.table;
    assert!(scene.rows.len() <= scene.viewport);
    assert!(scene.rows.len() < scene.total || scene.total <= scene.viewport);
    app.select_at(usize::MAX);
    draw(&mut app, 120, 14);
    assert!(app.scene.table.offset + app.scene.table.viewport >= app.scene.table.total);
}
