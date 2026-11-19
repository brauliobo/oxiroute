mod drawer;
mod footer;
mod hit;
mod overlay;
mod table;
mod tabs;
mod widths;

use ratatui::layout::Rect;

pub(crate) use drawer::DrawerScene;
pub(crate) use footer::ChipSlot;
pub(crate) use hit::Hit;
pub(crate) use overlay::Overlay;
pub(crate) use table::{ColumnSlot, RowScene, TableScene};
pub(crate) use tabs::TabsScene;
pub(crate) use widths::Widths;

use super::{
    app::App,
    layout::{Drawer, Layout, Meters, Summary},
    prefs::Panel,
};

#[derive(Default)]
pub(crate) struct Scene {
    pub(crate) layout: Option<Layout>,
    pub(crate) tabs: TabsScene,
    pub(crate) table: TableScene,
    pub(crate) chips: Vec<ChipSlot>,
    pub(crate) titles: Vec<(Rect, Panel)>,
    pub(crate) drawer: Option<DrawerScene>,
    pub(crate) overlay: Option<Overlay>,
}

impl Scene {
    pub(crate) fn build(app: &App, widths: &mut Widths) -> Self {
        let layout = app.layout();
        if layout.too_small {
            return Self {
                layout: Some(layout),
                ..Self::default()
            };
        }
        Self {
            tabs: tabs::build(app, &layout),
            table: table::build(app, &layout, widths),
            chips: footer::build(app, &layout),
            titles: titles(&layout),
            drawer: drawer::build(app, &layout),
            overlay: overlay::build(app, &layout),
            layout: Some(layout),
        }
    }
}

fn first_row(rect: Rect) -> Rect {
    Rect::new(rect.x, rect.y, rect.width, 1)
}

fn titles(layout: &Layout) -> Vec<(Rect, Panel)> {
    let mut titles = Vec::new();
    if let Some(meters) = layout.meters {
        let rect = match meters {
            Meters::Boxes(rect) | Meters::Strip(rect) | Meters::Title(rect) => rect,
        };
        titles.push((first_row(rect), Panel::Meters));
    }
    if let Some(summary) = layout.summary {
        let rect = match summary {
            Summary::Box(rect) | Summary::Title(rect) => rect,
        };
        titles.push((first_row(rect), Panel::Summary));
    }
    if let Some(drawer) = layout.drawer {
        let _: Drawer = drawer;
        titles.push((first_row(drawer.rect()), Panel::Drawer));
    }
    titles
}
