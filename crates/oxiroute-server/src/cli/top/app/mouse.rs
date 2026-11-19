use std::time::{Duration, Instant};

use ratatui::crossterm::event::{MouseButton, MouseEvent, MouseEventKind};

use super::{
    App, Layer,
    actions::{Action, Effect},
};
use crate::cli::top::{prefs::Panel, scene::Hit};

const DOUBLE_CLICK: Duration = Duration::from_millis(400);
const WHEEL_ROWS: isize = 3;

impl App {
    pub(crate) fn mouse(&mut self, event: MouseEvent) -> Effect {
        match event.kind {
            MouseEventKind::ScrollUp => self.scroll_rows(-WHEEL_ROWS),
            MouseEventKind::ScrollDown => self.scroll_rows(WHEEL_ROWS),
            MouseEventKind::Down(MouseButton::Left) => {
                let hit = self.scene.hit(event.column, event.row);
                self.click(hit)
            }
            _ => Effect::Idle,
        }
    }

    fn scroll_rows(&mut self, step: isize) -> Effect {
        if !self
            .layers
            .iter()
            .any(|layer| matches!(layer, Layer::Help(_) | Layer::Columns(_) | Layer::Views(_)))
        {
            self.move_selection(step);
        }
        Effect::Idle
    }

    fn click(&mut self, hit: Hit) -> Effect {
        match hit {
            Hit::Tab(view) => self.act(Action::GoTo(view)),
            Hit::Header(column) => self.act(Action::SortBy(column)),
            Hit::Marker(position) => self.act(Action::ClickMarker(position)),
            Hit::Row(position) => self.click_row(position),
            Hit::Chip(action) => self.act(action),
            Hit::Title(Panel::Drawer) if self.layers.contains(&Layer::Drawer) => {
                self.act(Action::Pop)
            }
            Hit::Title(Panel::Drawer) => self.act(Action::Detail),
            Hit::Title(panel) => self.act(Action::CollapsePanel(panel)),
            Hit::Item(item) => self.act(Action::PickerClick(item)),
            Hit::Outside => self.act(Action::Pop),
            Hit::Nothing => Effect::Idle,
        }
    }

    fn click_row(&mut self, position: usize) -> Effect {
        let double = self
            .clicked
            .is_some_and(|(previous, at)| previous == position && at.elapsed() < DOUBLE_CLICK);
        self.clicked = Some((position, Instant::now()));
        self.select_at(position);
        if double {
            self.act(Action::Activate)
        } else {
            Effect::Idle
        }
    }
}
