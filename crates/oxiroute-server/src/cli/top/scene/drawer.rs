use ratatui::layout::Rect;

use crate::cli::top::{
    app::App,
    layout::{Drawer, Layout},
};

#[derive(Clone, Debug)]
pub(crate) struct SparkLine {
    pub(crate) label: String,
    pub(crate) value: String,
    pub(crate) bars: Vec<u64>,
}

#[derive(Clone, Debug)]
pub(crate) struct DrawerScene {
    pub(crate) rect: Rect,
    pub(crate) open: bool,
    pub(crate) title: String,
    pub(crate) sparks: Vec<SparkLine>,
    pub(crate) lines: Vec<(&'static str, String)>,
    pub(crate) gone: bool,
}

pub(super) fn build(app: &App, layout: &Layout) -> Option<DrawerScene> {
    let drawer = layout.drawer?;
    let rect = drawer.rect();
    let label = app
        .arranged
        .as_ref()
        .and_then(|arranged| arranged.row(app.selection.position))
        .and_then(|row| row.cells.first())
        .map(|cell| cell.render(app.units()))
        .unwrap_or_default();
    if matches!(drawer, Drawer::Title(_)) {
        return Some(DrawerScene {
            rect,
            open: false,
            title: label,
            sparks: Vec::new(),
            lines: Vec::new(),
            gone: false,
        });
    }
    let key = app.selection.key.clone().unwrap_or_default();
    let sampling = app.sampling();
    let lines = sampling
        .as_ref()
        .map(|sampling| app.view.detail(sampling, &key).lines)
        .unwrap_or_default();
    let sparks = if app.selection.gone {
        Vec::new()
    } else {
        sparks(app)
    };
    Some(DrawerScene {
        rect,
        open: true,
        title: label,
        sparks,
        lines,
        gone: app.selection.gone,
    })
}

fn sparks(app: &App) -> Vec<SparkLine> {
    let Some(row) = app
        .arranged
        .as_ref()
        .and_then(|arranged| arranged.row(app.selection.position))
    else {
        return Vec::new();
    };
    app.follow
        .series
        .iter()
        .map(|(title, _)| {
            let value = app
                .view
                .columns()
                .iter()
                .position(|column| column.title == *title)
                .map(|index| row.cells[index].render(app.units()))
                .unwrap_or_default();
            SparkLine {
                label: (*title).to_owned(),
                value,
                bars: app.follow.bars(title),
            }
        })
        .collect()
}
