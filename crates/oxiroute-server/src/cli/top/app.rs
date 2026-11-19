mod actions;
mod browse;
mod follow;
mod keymap;
mod keys;
mod layers;
mod mouse;
mod picker;
mod selection;

use std::{
    collections::{BTreeMap, BTreeSet},
    time::Instant,
};

use ratatui::layout::Rect;

pub(crate) use actions::{Action, Effect};
pub(crate) use follow::Follow;
pub(crate) use keymap::{Scope, chips, help_lines};
pub(crate) use keys::Key;
pub(crate) use layers::{Crumb, Layer};
pub(crate) use selection::Selection;

use super::{
    history::{History, Pushed, Sampling, Window},
    layout::{DrawerMode, Inputs, Layout, Tier},
    model::{Arranged, Arrangement, Filter, Groups, Sort, Table, Units},
    prefs::{PanelState, Prefs},
    scene::{Scene, Widths},
    source::{Sample, Wants},
    theme::{Capabilities, Theme},
    views::View,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Control {
    pub(crate) paused: bool,
    pub(crate) wants: Wants,
}

pub(crate) struct Settings {
    pub(crate) view: View,
    pub(crate) sort: Option<Sort>,
    pub(crate) filter: String,
    pub(crate) limit: usize,
    pub(crate) mouse: bool,
    pub(crate) prefs: Prefs,
    pub(crate) window: Window,
    pub(crate) capabilities: Capabilities,
    pub(crate) notice: Option<String>,
}

pub(crate) struct App {
    pub(crate) prefs: Prefs,
    pub(crate) prefs_dirty: bool,
    pub(crate) capabilities: Capabilities,
    pub(crate) theme: Theme,
    pub(crate) view: View,
    pub(crate) sorts: BTreeMap<View, Sort>,
    pub(crate) filter: Filter,
    pub(crate) history: History,
    pub(crate) window: Window,
    pub(crate) switches: Switches,
    pub(crate) lock: Option<Vec<String>>,
    pub(crate) grouped: BTreeSet<View>,
    pub(crate) groups: BTreeMap<View, Groups>,
    pub(crate) selection: Selection,
    pub(crate) scroll: usize,
    pub(crate) layers: Vec<Layer>,
    pub(crate) trail: Vec<Crumb>,
    pub(crate) message: Option<String>,
    pub(crate) error: Option<String>,
    pub(crate) area: Rect,
    pub(crate) arranged: Option<Arranged>,
    pub(crate) follow: Follow,
    pub(crate) scene: Scene,
    pub(crate) limit: usize,
    pub(crate) clicked: Option<(usize, Instant)>,
    pending: Pending,
    widths: Widths,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Switches {
    pub(crate) paused: bool,
    pub(crate) mouse: bool,
    pub(crate) focus: bool,
}

#[derive(Clone, Copy, Debug)]
struct Pending {
    arrange: bool,
    follow: bool,
}

impl App {
    pub(crate) fn new(settings: Settings) -> Self {
        let Settings {
            view,
            sort,
            filter,
            limit,
            mouse,
            prefs,
            window,
            capabilities,
            notice,
        } = settings;
        let mut sorts = BTreeMap::new();
        if let Some(sort) = sort {
            sorts.insert(view, sort);
        }
        let theme = Theme::new(prefs.theme, capabilities);
        let mut app = Self {
            prefs,
            prefs_dirty: false,
            capabilities,
            theme,
            view,
            sorts,
            filter: Filter::parse(&filter),
            history: History::default(),
            window,
            switches: Switches {
                paused: false,
                mouse,
                focus: false,
            },
            lock: None,
            grouped: BTreeSet::new(),
            groups: BTreeMap::new(),
            selection: Selection::default(),
            scroll: 0,
            layers: Vec::new(),
            trail: Vec::new(),
            message: notice,
            error: None,
            area: Rect::new(0, 0, 80, 24),
            arranged: None,
            follow: Follow::default(),
            scene: Scene::default(),
            limit,
            clicked: None,
            pending: Pending {
                arrange: true,
                follow: false,
            },
            widths: Widths::default(),
        };
        app.prefs.window = window;
        app
    }

    pub(crate) fn tier(&self) -> Tier {
        Tier::of(self.area.width)
    }

    pub(crate) fn units(&self) -> Units {
        self.prefs.units
    }

    pub(crate) fn sort(&self) -> Sort {
        self.sorts
            .get(&self.view)
            .copied()
            .unwrap_or_else(|| self.view.default_sort())
    }

    pub(crate) fn visible_views(&self) -> Vec<View> {
        self.prefs.views.visible()
    }

    pub(crate) fn groups_of(&self, view: View) -> Groups {
        self.groups
            .get(&view)
            .cloned()
            .unwrap_or_else(|| Groups::new(true))
    }

    pub(crate) fn drawer_mode(&self) -> DrawerMode {
        if self.layers.contains(&Layer::Drawer) {
            DrawerMode::Open
        } else if self.prefs.panels.drawer == PanelState::Hidden {
            DrawerMode::Hidden
        } else {
            DrawerMode::Collapsed
        }
    }

    pub(crate) fn layout_inputs(&self) -> Inputs {
        Inputs {
            area: self.area,
            density: self.prefs.density,
            focus: self.switches.focus,
            meters: self.prefs.panels.meters,
            summary: self.prefs.panels.summary,
            drawer: self.drawer_mode(),
        }
    }

    pub(crate) fn layout(&self) -> Layout {
        Layout::compute(&self.layout_inputs())
    }

    pub(crate) fn control(&self) -> Control {
        let key = self.view.columns()[self.sort().column].key;
        Control {
            paused: self.switches.paused,
            wants: Wants::for_view(self.view, key, &self.filter, self.limit),
        }
    }

    pub(crate) fn sample(&mut self, sample: Result<Sample, String>) {
        if self.switches.paused {
            return;
        }
        match sample {
            Ok(sample) => {
                if self.history.push(sample) == Pushed::Reset {
                    self.message =
                        Some("the server restarted or reloaded: rate history cleared".into());
                }
                self.error = None;
                self.pending = Pending {
                    arrange: true,
                    follow: true,
                };
            }
            Err(error) => self.error = Some(error),
        }
    }

    pub(crate) fn invalidate(&mut self) {
        self.pending.arrange = true;
    }

    pub(crate) fn sampling(&self) -> Option<Sampling<'_>> {
        self.history.sampling(self.window, self.view)
    }

    pub(crate) fn build_table(&self) -> Option<Table> {
        let table = self.view.table(&self.sampling()?);
        Some(
            match self
                .view
                .grouping()
                .filter(|_| self.grouped.contains(&self.view))
            {
                Some(key) => table.grouped(key),
                None => table,
            },
        )
    }

    pub(crate) fn rearrange(&mut self) {
        let Some(table) = self.build_table() else {
            self.arranged = None;
            return;
        };
        let groups = self.groups_of(self.view);
        let arrangement = Arrangement {
            sort: self.sort(),
            filter: &self.filter,
            groups: &groups,
            lock: self.lock.as_deref(),
            units: self.units(),
        };
        self.arranged = Some(Arranged::new(table, &arrangement));
    }

    pub(crate) fn prepare(&mut self, area: Rect) {
        if area != self.area {
            self.area = area;
            self.widths = Widths::default();
        }
        if std::mem::take(&mut self.pending.arrange) {
            self.rearrange();
            self.reselect();
            if std::mem::take(&mut self.pending.follow) {
                self.record_follow();
            }
        }
        let mut widths = std::mem::take(&mut self.widths);
        let scene = Scene::build(self, &mut widths);
        self.widths = widths;
        self.scroll = scene.table.offset;
        self.scene = scene;
    }

    pub(crate) fn reset_widths(&mut self) {
        self.widths = Widths::default();
    }

    pub(crate) fn changed_prefs(&mut self) {
        self.prefs_dirty = true;
    }
}
