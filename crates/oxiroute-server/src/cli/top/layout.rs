mod fit;
mod tier;

use ratatui::layout::Rect;

pub(crate) use fit::{Candidate, fit};
pub(crate) use tier::Tier;

use super::prefs::{Density, PanelState};

pub(crate) const MIN_WIDTH: u16 = 60;
pub(crate) const MIN_HEIGHT: u16 = 12;
const MIN_BODY: u16 = 8;
const SIDE_DRAWER: u16 = 42;
const BOTTOM_DRAWER: u16 = 9;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DrawerMode {
    Open,
    Collapsed,
    Hidden,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Inputs {
    pub(crate) area: Rect,
    pub(crate) density: Density,
    pub(crate) focus: bool,
    pub(crate) meters: PanelState,
    pub(crate) summary: PanelState,
    pub(crate) drawer: DrawerMode,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Meters {
    Boxes(Rect),
    Strip(Rect),
    Title(Rect),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Summary {
    Box(Rect),
    Title(Rect),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Drawer {
    Side(Rect),
    Bottom(Rect),
    Title(Rect),
}

impl Drawer {
    pub(crate) const fn rect(self) -> Rect {
        match self {
            Self::Side(rect) | Self::Bottom(rect) | Self::Title(rect) => rect,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Layout {
    pub(crate) area: Rect,
    pub(crate) tier: Tier,
    pub(crate) too_small: bool,
    pub(crate) status: Option<Rect>,
    pub(crate) meters: Option<Meters>,
    pub(crate) summary: Option<Summary>,
    pub(crate) tabs: Rect,
    pub(crate) table: Rect,
    pub(crate) bordered: bool,
    pub(crate) drawer: Option<Drawer>,
    pub(crate) message: Rect,
    pub(crate) keys: Rect,
}

fn strip(area: Rect, top: &mut u16, height: u16) -> Rect {
    let rect = Rect::new(area.x, area.y + *top, area.width, height);
    *top += height;
    rect
}

impl Layout {
    pub(crate) fn compute(inputs: &Inputs) -> Self {
        let area = inputs.area;
        let tier = Tier::of(area.width);
        let too_small = area.width < MIN_WIDTH || area.height < MIN_HEIGHT;
        let chrome = !inputs.focus && inputs.density == Density::Normal;
        let status_rows = u16::from(!inputs.focus);
        let mut meters_rows = if chrome {
            meters_height(inputs.meters, tier)
        } else {
            0
        };
        let mut summary_rows = if chrome {
            summary_height(inputs.summary, tier)
        } else {
            0
        };
        let fixed = |meters: u16, summary: u16| status_rows + meters + summary + 1 + 2;
        while area.height.saturating_sub(fixed(meters_rows, summary_rows)) < MIN_BODY
            && (meters_rows + summary_rows) > 0
        {
            if summary_rows > 0 {
                summary_rows = 0;
            } else {
                meters_rows = u16::from(meters_rows > 1);
            }
        }
        let mut top = 0;
        let status = (status_rows > 0).then(|| strip(area, &mut top, 1));
        let meters = (meters_rows > 0).then(|| strip(area, &mut top, meters_rows));
        let summary = (summary_rows > 0).then(|| strip(area, &mut top, summary_rows));
        let tabs = strip(area, &mut top, 1);
        let body_height = area.height.saturating_sub(top + 2);
        let body = Rect::new(area.x, area.y + top, area.width, body_height);
        let message = Rect::new(
            area.x,
            area.y + area.height.saturating_sub(2),
            area.width,
            1,
        );
        let keys = Rect::new(
            area.x,
            area.y + area.height.saturating_sub(1),
            area.width,
            1,
        );
        let (table, drawer) = split_body(body, inputs);
        Self {
            area,
            tier,
            too_small,
            status,
            meters: meters.map(|rect| match (inputs.meters, rect.height) {
                (PanelState::Collapsed, _) => Meters::Title(rect),
                (_, 1) => Meters::Strip(rect),
                _ => Meters::Boxes(rect),
            }),
            summary: summary.map(|rect| match inputs.summary {
                PanelState::Collapsed => Summary::Title(rect),
                _ if rect.height == 1 => Summary::Title(rect),
                _ => Summary::Box(rect),
            }),
            tabs,
            table,
            bordered: inputs.density == Density::Normal,
            drawer,
            message,
            keys,
        }
    }

    pub(crate) fn table_inner(&self) -> Rect {
        if self.bordered {
            Rect::new(
                self.table.x + 1,
                self.table.y + 1,
                self.table.width.saturating_sub(2),
                self.table.height.saturating_sub(2),
            )
        } else {
            self.table
        }
    }
}

fn meters_height(state: PanelState, tier: Tier) -> u16 {
    match (state, tier) {
        (PanelState::Hidden, _) => 0,
        (PanelState::Expanded, Tier::Standard | Tier::Wide) => 5,
        (PanelState::Collapsed | PanelState::Expanded, _) => 1,
    }
}

fn summary_height(state: PanelState, tier: Tier) -> u16 {
    match (state, tier) {
        (PanelState::Hidden, _) => 0,
        (PanelState::Collapsed, _) | (_, Tier::Minimal) => 1,
        _ => 3,
    }
}

fn split_body(body: Rect, inputs: &Inputs) -> (Rect, Option<Drawer>) {
    if inputs.focus {
        return (body, None);
    }
    match inputs.drawer {
        DrawerMode::Collapsed => {
            let table = Rect::new(body.x, body.y, body.width, body.height.saturating_sub(1));
            let title = Rect::new(body.x, body.y + table.height, body.width, 1);
            (table, Some(Drawer::Title(title)))
        }
        DrawerMode::Open if body.width >= 120 => {
            let table = Rect::new(body.x, body.y, body.width - SIDE_DRAWER, body.height);
            let side = Rect::new(body.x + table.width, body.y, SIDE_DRAWER, body.height);
            (table, Some(Drawer::Side(side)))
        }
        DrawerMode::Open if body.height >= BOTTOM_DRAWER + MIN_BODY => {
            let table = Rect::new(body.x, body.y, body.width, body.height - BOTTOM_DRAWER);
            let bottom = Rect::new(body.x, body.y + table.height, body.width, BOTTOM_DRAWER);
            (table, Some(Drawer::Bottom(bottom)))
        }
        DrawerMode::Open | DrawerMode::Hidden => (body, None),
    }
}
