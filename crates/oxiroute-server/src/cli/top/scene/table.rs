use ratatui::layout::Rect;

use super::Widths;
use crate::cli::{
    top::{
        app::App,
        layout::{Candidate, Layout, fit},
        model::{Arranged, Notice, Row, Tone},
    },
    units,
};

const GUTTER: u16 = 2;
const TEXT_CAP: usize = 40;
const LABEL_CAP: usize = 44;

type Cells = Vec<(String, Tone)>;

#[derive(Clone, Debug)]
pub(crate) struct ColumnSlot {
    pub(crate) index: usize,
    pub(crate) x: u16,
    pub(crate) width: u16,
    pub(crate) numeric: bool,
    pub(crate) title: String,
    pub(crate) sorted: Option<bool>,
}

#[derive(Clone, Debug)]
pub(crate) struct RowScene {
    pub(crate) position: Option<usize>,
    pub(crate) y: u16,
    pub(crate) cells: Cells,
    pub(crate) selected: bool,
    pub(crate) marker: Option<Rect>,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct TableScene {
    pub(crate) outer: Rect,
    pub(crate) inner: Rect,
    pub(crate) title: String,
    pub(crate) notice: Option<String>,
    pub(crate) columns: Vec<ColumnSlot>,
    pub(crate) rows: Vec<RowScene>,
    pub(crate) pinned: Vec<RowScene>,
    pub(crate) offset: usize,
    pub(crate) viewport: usize,
    pub(crate) total: usize,
    pub(crate) body: Rect,
}

struct Painted<'a> {
    position: Option<usize>,
    row: &'a Row,
    cells: Cells,
}

struct Geometry {
    inner: Rect,
    pinned: usize,
    viewport: usize,
    offset: usize,
    body: Rect,
}

fn clip(text: &str, width: usize, ellipsis: &str, numeric: bool) -> String {
    if text.chars().count() <= width || numeric {
        return text.to_owned();
    }
    let keep = width.saturating_sub(ellipsis.chars().count());
    format!("{}{ellipsis}", text.chars().take(keep).collect::<String>())
}

fn label(app: &App, arranged: &Arranged, position: usize, row: &Row, text: &str) -> String {
    if !arranged.parents.iter().any(|parent| *parent) && row.depth == 0 {
        return text.to_owned();
    }
    let symbols = app.theme.symbols;
    let glyph = if arranged.parents[arranged.order[position]] {
        let open = !app.filter.is_empty() || app.groups_of(app.view).is_open(&row.key);
        if open { symbols.open } else { symbols.closed }
    } else {
        symbols.leaf
    };
    format!("{}{glyph} {text}", "  ".repeat(usize::from(row.depth)))
}

fn paint(app: &App, columns: &[usize], row: &Row) -> Cells {
    columns
        .iter()
        .map(|index| {
            (
                row.cells[*index].render(app.units()),
                row.cells[*index].tone,
            )
        })
        .collect()
}

fn title(app: &App, arranged: Option<&Arranged>) -> String {
    let mut title = app.view.title().to_owned();
    let Some(arranged) = arranged else {
        return title;
    };
    let table = &arranged.table;
    let rows = table.rows.iter().filter(|row| row.parent.is_none()).count();
    match table.population {
        Some(population) if population.of > population.shown => {
            title = format!(
                "{title} (top {} of {})",
                population.shown,
                units::count(population.of)
            );
        }
        _ => title = format!("{title} ({rows})"),
    }
    if let Some(sampling) = app.sampling() {
        let fill = if sampling.complete { "" } else { " filling" };
        title = format!("{title} {} rates{fill}", app.window.label());
    }
    if !app.filter.is_empty() {
        title = format!(
            "{title} filter matches {} of {}",
            arranged.len(),
            table.rows.len()
        );
        if table.total.is_some() || table.other.is_some() {
            title.push_str(", totals cover all");
        }
    }
    title
}

fn pinned_rows(arranged: Option<&Arranged>) -> Vec<&Row> {
    arranged.map_or_else(Vec::new, |arranged| {
        [arranged.table.other.as_ref(), arranged.table.total.as_ref()]
            .into_iter()
            .flatten()
            .collect()
    })
}

fn geometry(app: &App, layout: &Layout, arranged: Option<&Arranged>) -> Geometry {
    let inner = layout.table_inner();
    let pinned = pinned_rows(arranged)
        .len()
        .min(usize::from(inner.height.saturating_sub(3)));
    let viewport = usize::from(inner.height.saturating_sub(1)).saturating_sub(pinned);
    let len = arranged.map_or(0, Arranged::len);
    let selected = app.selection.position;
    let mut offset = app.scroll.min(len.saturating_sub(viewport));
    if selected < offset {
        offset = selected;
    } else if viewport > 0 && selected >= offset + viewport {
        offset = selected + 1 - viewport;
    }
    let body = Rect::new(
        inner.x,
        inner.y + 1,
        inner.width,
        u16::try_from(viewport).unwrap_or(0),
    );
    Geometry {
        inner,
        pinned,
        viewport,
        offset,
        body,
    }
}

fn candidates(
    app: &App,
    columns: &[usize],
    painted: &[Painted],
    tail: &[Painted],
    widths: &mut Widths,
) -> Vec<Candidate> {
    let definitions = app.view.columns();
    let prefs = app.prefs.columns_of(app.view);
    columns
        .iter()
        .enumerate()
        .map(|(slot, index)| {
            let definition = &definitions[*index];
            let content = painted
                .iter()
                .chain(tail)
                .map(|row| row.cells[slot].0.chars().count())
                .max()
                .unwrap_or(0);
            let cap = match (slot, definition.numeric) {
                (0, _) => LABEL_CAP,
                (_, true) => usize::MAX,
                _ => TEXT_CAP,
            };
            let wanted = content.min(cap).max(definition.title.chars().count() + 1);
            Candidate {
                index: *index,
                width: widths.settle(*index, u16::try_from(wanted).unwrap_or(u16::MAX)),
                priority: definition.priority,
                forced: prefs.forced(app.view, *index),
            }
        })
        .collect()
}

fn row_scene(
    arranged: &Arranged,
    geometry: &Geometry,
    painted: &Painted,
    kept: &[usize],
    selected: usize,
) -> RowScene {
    let cells = kept
        .iter()
        .map(|slot| painted.cells[*slot].clone())
        .collect();
    let Some(position) = painted.position else {
        return RowScene {
            position: None,
            y: 0,
            cells,
            selected: false,
            marker: None,
        };
    };
    let y = geometry.body.y + u16::try_from(position - geometry.offset).unwrap_or(0);
    let parent = arranged.parents[arranged.order[position]];
    let label_x = geometry.inner.x + GUTTER;
    RowScene {
        position: Some(position),
        y,
        cells,
        selected: position == selected,
        marker: parent.then(|| Rect::new(label_x + u16::from(painted.row.depth) * 2, y, 2, 1)),
    }
}

pub(super) fn build(app: &App, layout: &Layout, widths: &mut Widths) -> TableScene {
    let arranged = app.arranged.as_ref();
    let geometry = geometry(app, layout, arranged);
    let inner = geometry.inner;
    let mut scene = TableScene {
        outer: layout.table,
        inner,
        title: title(app, arranged),
        offset: geometry.offset,
        viewport: geometry.viewport,
        total: arranged.map_or(0, Arranged::len),
        body: geometry.body,
        ..TableScene::default()
    };
    let Some(arranged) = arranged else {
        scene.notice = Some("waiting for the first snapshot".into());
        return scene;
    };
    scene.notice = arranged.table.notice.as_ref().map(Notice::text);
    let columns = app.prefs.visible(app.view, layout.tier);
    let last = (geometry.offset + geometry.viewport).min(arranged.len());
    let mut painted = (geometry.offset..last)
        .filter_map(|position| arranged.row(position).map(|row| (position, row)))
        .map(|(position, row)| {
            let mut cells = paint(app, &columns, row);
            cells[0].0 = label(app, arranged, position, row, &cells[0].0);
            Painted {
                position: Some(position),
                row,
                cells,
            }
        })
        .collect::<Vec<_>>();
    let pinned = pinned_rows(Some(arranged));
    let mut tail = pinned[pinned.len() - geometry.pinned..]
        .iter()
        .map(|row| Painted {
            position: None,
            row,
            cells: paint(app, &columns, row),
        })
        .collect::<Vec<_>>();
    let scrollbar = u16::from(arranged.len() > geometry.viewport && geometry.viewport > 0);
    let available = inner.width.saturating_sub(GUTTER + scrollbar);
    let fitted = fit(
        candidates(app, &columns, &painted, &tail, widths),
        available,
    );
    let kept = fitted
        .iter()
        .filter_map(|candidate| columns.iter().position(|index| *index == candidate.index))
        .collect::<Vec<_>>();
    let definitions = app.view.columns();
    let sort = app.sort();
    let ellipsis = app.theme.symbols.ellipsis;
    let mut x = inner.x + GUTTER;
    for (candidate, slot) in fitted.iter().zip(&kept) {
        let definition = &definitions[candidate.index];
        scene.columns.push(ColumnSlot {
            index: candidate.index,
            x,
            width: candidate.width,
            numeric: definition.numeric,
            title: definition.title.to_owned(),
            sorted: (candidate.index == sort.column).then_some(sort.descending),
        });
        for row in painted.iter_mut().chain(tail.iter_mut()) {
            let cell = &mut row.cells[*slot];
            cell.0 = clip(
                &cell.0,
                usize::from(candidate.width),
                ellipsis,
                definition.numeric,
            );
        }
        x += candidate.width + 1;
    }
    let selected = app.selection.position;
    scene.rows = painted
        .iter()
        .map(|row| row_scene(arranged, &geometry, row, &kept, selected))
        .collect();
    let pinned_y = inner.y + inner.height - u16::try_from(tail.len()).unwrap_or(0);
    scene.pinned = tail
        .iter()
        .enumerate()
        .map(|(index, row)| RowScene {
            y: pinned_y + u16::try_from(index).unwrap_or(0),
            ..row_scene(arranged, &geometry, row, &kept, selected)
        })
        .collect();
    scene
}
