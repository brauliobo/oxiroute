use ratatui::layout::Rect;

use crate::cli::top::{
    app::{App, Layer, help_lines},
    layout::Layout,
    views::View,
};

#[derive(Clone, Debug)]
pub(crate) struct OverlayItem {
    pub(crate) index: usize,
    pub(crate) rect: Rect,
    pub(crate) text: String,
    pub(crate) on: Option<bool>,
    pub(crate) cursor: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct Overlay {
    pub(crate) rect: Rect,
    pub(crate) title: String,
    pub(crate) items: Vec<OverlayItem>,
    pub(crate) clickable: bool,
}

struct Entry {
    text: String,
    on: Option<bool>,
}

fn entries(app: &App, layout: &Layout, layer: &Layer) -> (String, Vec<Entry>, usize) {
    match layer {
        Layer::Columns(cursor) => {
            let prefs = app.prefs.columns_of(app.view);
            let columns = app.view.columns();
            let rows = prefs
                .ordered(app.view)
                .into_iter()
                .map(|index| Entry {
                    text: format!(
                        "{:<10} P{}  {}",
                        columns[index].title, columns[index].priority, columns[index].key
                    ),
                    on: Some(prefs.is_on(app.view, index, layout.tier)),
                })
                .collect();
            (
                format!("Columns of {} (small defaults per width)", app.view.title()),
                rows,
                *cursor,
            )
        }
        Layer::Views(cursor) => {
            let rows = app
                .prefs
                .views
                .ordered()
                .into_iter()
                .map(|view: View| Entry {
                    text: view.title().to_owned(),
                    on: Some(!app.prefs.views.is_hidden(view)),
                })
                .collect();
            (
                "Views (hidden ones stay reachable here)".to_owned(),
                rows,
                *cursor,
            )
        }
        _ => {
            let rows = help_lines()
                .into_iter()
                .map(|(keys, help)| Entry {
                    text: format!("{keys:<18} {help}"),
                    on: None,
                })
                .collect();
            let offset = if let Layer::Help(offset) = layer {
                *offset
            } else {
                0
            };
            ("Keys (Up/Down scroll, Esc closes)".to_owned(), rows, offset)
        }
    }
}

pub(super) fn build(app: &App, layout: &Layout) -> Option<Overlay> {
    let layer = app
        .layers
        .iter()
        .rev()
        .find(|layer| matches!(layer, Layer::Help(_) | Layer::Columns(_) | Layer::Views(_)))?;
    let (title, rows, cursor) = entries(app, layout, layer);
    let area = layout.area;
    let width = area.width.saturating_sub(4).min(76);
    let wanted = u16::try_from(rows.len())
        .unwrap_or(u16::MAX)
        .saturating_add(2);
    let height = wanted.min(area.height.saturating_sub(2));
    let rect = Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    );
    let visible = usize::from(height.saturating_sub(2));
    let first = if matches!(layer, Layer::Help(_)) {
        cursor.min(rows.len().saturating_sub(visible))
    } else {
        (cursor + 1).saturating_sub(visible)
    };
    let items = rows
        .into_iter()
        .enumerate()
        .skip(first)
        .take(visible)
        .map(|(index, entry)| OverlayItem {
            index,
            rect: Rect::new(
                rect.x + 1,
                rect.y + 1 + u16::try_from(index - first).unwrap_or(0),
                width.saturating_sub(2),
                1,
            ),
            text: entry.text,
            on: entry.on,
            cursor: index == cursor && !matches!(layer, Layer::Help(_)),
        })
        .collect();
    Some(Overlay {
        rect,
        title,
        items,
        clickable: !matches!(layer, Layer::Help(_)),
    })
}
