use ratatui::layout::Rect;

use crate::cli::top::{
    app::App,
    layout::{Layout, Tier},
    model::Tone,
    views::View,
};

#[derive(Clone, Debug)]
pub(crate) struct TabSlot {
    pub(crate) rect: Rect,
    pub(crate) view: View,
    pub(crate) number: usize,
    pub(crate) badge: Option<(String, Tone)>,
    pub(crate) active: bool,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct TabsScene {
    pub(crate) slots: Vec<TabSlot>,
    pub(crate) status: String,
    pub(crate) status_rect: Rect,
}

fn tab_width(view: View, number: usize, badge: Option<&(String, Tone)>) -> u16 {
    let label = format!("{number} {}", view.title()).chars().count();
    let badge = badge.map_or(0, |(text, _)| text.chars().count() + 1);
    u16::try_from(label + badge + 2).unwrap_or(u16::MAX)
}

pub(super) fn build(app: &App, layout: &Layout) -> TabsScene {
    let area = layout.tabs;
    let sample = app.history.latest();
    let show_badges = layout.tier > Tier::Minimal;
    let status = status_text(app);
    let status_width = u16::try_from(status.chars().count())
        .unwrap_or(u16::MAX)
        .min(area.width / 2);
    let limit = area.x + area.width.saturating_sub(status_width + 1);
    let mut slots = Vec::new();
    let mut x = area.x;
    for (index, view) in app.visible_views().into_iter().enumerate() {
        let badge = sample
            .filter(|_| show_badges)
            .and_then(|sample| view.badge(sample));
        let width = tab_width(view, index + 1, badge.as_ref());
        if x + width > limit {
            break;
        }
        slots.push(TabSlot {
            rect: Rect::new(x, area.y, width, 1),
            view,
            number: index + 1,
            badge,
            active: view == app.view,
        });
        x += width + 1;
    }
    TabsScene {
        slots,
        status_rect: Rect::new(area.x + area.width - status_width, area.y, status_width, 1),
        status,
    }
}

fn status_text(app: &App) -> String {
    let symbols = app.theme.symbols;
    let sort = app.sort();
    let column = app.view.columns()[sort.column].title;
    let arrow = if sort.descending {
        symbols.descending
    } else {
        symbols.ascending
    };
    let mut parts = vec![format!("sort {column}{arrow}")];
    if !app.filter.is_empty() {
        parts.push(format!("filter {}", app.filter.source()));
    }
    if app.grouped.contains(&app.view) {
        parts.push("grouped".into());
    }
    if app.lock.is_some() {
        parts.push("order frozen".into());
    }
    if !app.trail.is_empty() {
        let crumbs = app
            .trail
            .iter()
            .map(|crumb| crumb.view.title())
            .collect::<Vec<_>>();
        parts.insert(0, format!("{} > {}", crumbs.join(" > "), app.view.title()));
    }
    parts.join("  ")
}
