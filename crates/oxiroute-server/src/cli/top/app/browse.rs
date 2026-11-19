use super::{
    App, Layer,
    actions::{Action, Effect},
    keymap::{self, Scope},
    keys::Key,
};
use crate::cli::top::{
    model::{Filter, Groups, Sort},
    prefs::{Panel, PanelState},
    theme::Theme,
    views::View,
};

impl App {
    pub(crate) fn key(&mut self, key: Key) -> Effect {
        if key == Key::Ctrl('c') {
            return Effect::Quit;
        }
        let scope = self.layers.last().map_or(Scope::Browse, Layer::scope);
        if let (Scope::Filter, Key::Char(character)) = (scope, key) {
            self.type_into_filter(character);
            return Effect::Idle;
        }
        if scope == Scope::Browse && key == Key::Escape {
            return self.act(Action::Pop);
        }
        keymap::lookup(scope, key).map_or(Effect::Idle, |action| self.act(action))
    }

    pub(crate) fn act(&mut self, action: Action) -> Effect {
        self.message = None;
        match action {
            Action::Quit => return Effect::Quit,
            Action::ToggleMouse => {
                self.switches.mouse = !self.switches.mouse;
                return Effect::Mouse(self.switches.mouse);
            }
            Action::Help => self.layers.push(Layer::Help(0)),
            Action::Pop => self.pop(),
            Action::Pause => self.switches.paused = !self.switches.paused,
            Action::OpenFilter => self
                .layers
                .push(Layer::Filter(self.filter.source().to_owned())),
            Action::OpenColumns => self.layers.push(Layer::Columns(0)),
            Action::OpenViews => self.open_views(),
            Action::FilterEnter => {
                self.layers.pop();
            }
            Action::FilterClear => {
                self.layers.pop();
                self.set_filter("");
            }
            Action::FilterWipe => self.edit_draft(String::clear),
            Action::FilterErase => self.edit_draft(|draft| {
                draft.pop();
            }),
            Action::PickerUp
            | Action::PickerDown
            | Action::PickerToggle
            | Action::PickerMoveUp
            | Action::PickerMoveDown
            | Action::PickerReset
            | Action::PickerClick(_) => self.pick(action),
            _ => self.browse(action),
        }
        Effect::Idle
    }

    fn browse(&mut self, action: Action) {
        match action {
            Action::CycleWindow => self.cycle_window(),
            Action::ToggleUnits => {
                self.prefs.units = self.prefs.units.toggled();
                self.edited_prefs();
            }
            Action::CycleTheme => {
                self.prefs.theme = self.prefs.theme.next();
                self.theme = Theme::new(self.prefs.theme, self.capabilities);
                self.message = Some(format!("theme {}", self.prefs.theme.label()));
                self.changed_prefs();
            }
            Action::ToggleDensity => {
                self.prefs.density = self.prefs.density.toggled();
                self.edited_prefs();
            }
            Action::ToggleFocus => self.switches.focus = !self.switches.focus,
            Action::LockOrder => self.lock_order(),
            Action::CollapsePanel(Panel::Drawer) | Action::Detail => self.open_drawer(),
            Action::CollapsePanel(panel) => self.set_panel(panel, PanelState::collapse_toggled),
            Action::HidePanel(panel) => self.set_panel(panel, PanelState::hide_toggled),
            Action::RestorePanel(panel) => self.set_panel(panel, |_| {
                if panel == Panel::Drawer {
                    PanelState::Collapsed
                } else {
                    PanelState::Expanded
                }
            }),
            Action::NextView => self.step_view(1),
            Action::PreviousView => self.step_view(-1),
            Action::Slot(slot) => {
                if let Some(view) = self.visible_views().get(slot).copied() {
                    self.switch_view(view);
                }
            }
            Action::GoTo(view) => self.switch_view(view),
            Action::SortNext => self.step_sort(1),
            Action::SortPrevious => self.step_sort(-1),
            Action::SortReverse => self.reverse_sort(),
            Action::SortBy(column) => self.sort_by(column),
            Action::Up => self.move_selection(-1),
            Action::Down => self.move_selection(1),
            Action::PageUp => self.move_selection(-self.page()),
            Action::PageDown => self.move_selection(self.page()),
            Action::Top => self.select_at(0),
            Action::Bottom => self.select_at(usize::MAX),
            Action::Activate if self.selected_is_parent() => self.toggle_node(None),
            Action::Activate => self.open_drawer(),
            Action::ToggleNode => self.toggle_node(None),
            Action::OpenNode => self.toggle_node(Some(true)),
            Action::CloseNode => self.close_node(),
            Action::ExpandAll => self.every_group(Groups::open_all),
            Action::CollapseAll => self.every_group(Groups::close_all),
            Action::ToggleGrouping => self.toggle_grouping(),
            Action::Drill => self.drill(),
            Action::ClickMarker(position) => {
                self.select_at(position);
                self.toggle_node(None);
            }
            _ => {}
        }
    }

    fn page(&self) -> isize {
        isize::try_from(self.viewport().saturating_sub(1).max(1)).unwrap_or(1)
    }

    fn edited_prefs(&mut self) {
        self.changed_prefs();
        self.reset_widths();
        self.invalidate();
    }

    fn cycle_window(&mut self) {
        self.window = self.window.next();
        self.prefs.window = self.window;
        self.changed_prefs();
        self.invalidate();
    }

    fn lock_order(&mut self) {
        self.lock = match (&self.lock, &self.arranged) {
            (None, Some(arranged)) => Some(arranged.keys()),
            _ => None,
        };
        self.message = Some(
            if self.lock.is_some() {
                "row order frozen"
            } else {
                "row order released"
            }
            .into(),
        );
        self.invalidate();
    }

    fn set_panel(&mut self, panel: Panel, change: impl FnOnce(PanelState) -> PanelState) {
        let state = change(self.prefs.panels.get(panel));
        self.prefs.panels.set(panel, state);
        if state == PanelState::Hidden {
            self.layers.retain(|layer| *layer != Layer::Drawer);
        }
        self.changed_prefs();
    }

    fn open_drawer(&mut self) {
        if self.prefs.panels.drawer == PanelState::Hidden {
            self.message = Some("the detail drawer is hidden: press I to restore it".into());
        } else if self.selection.key.is_some() && !self.layers.contains(&Layer::Drawer) {
            self.layers.push(Layer::Drawer);
            self.record_follow();
        }
    }

    fn open_views(&mut self) {
        let at = self
            .prefs
            .views
            .ordered()
            .iter()
            .position(|view| *view == self.view)
            .unwrap_or(0);
        self.layers.push(Layer::Views(at));
    }

    fn pop(&mut self) {
        if self.layers.pop().is_some() {
            return;
        }
        if let Some(crumb) = self.trail.pop() {
            self.view = crumb.view;
            self.set_filter(&crumb.filter);
            self.selection = super::Selection {
                key: crumb.selected,
                ..super::Selection::default()
            };
            self.after_navigation();
        } else if !self.filter.is_empty() {
            self.set_filter("");
        }
    }

    pub(crate) fn set_filter(&mut self, source: &str) {
        self.filter = Filter::parse(source);
        self.reset_widths();
        self.invalidate();
    }

    fn type_into_filter(&mut self, character: char) {
        self.edit_draft(|draft| draft.push(character));
    }

    fn edit_draft(&mut self, edit: impl FnOnce(&mut String)) {
        let Some(Layer::Filter(draft)) = self.layers.last_mut() else {
            return;
        };
        edit(draft);
        let source = draft.clone();
        self.set_filter(&source);
    }

    pub(crate) fn switch_view(&mut self, view: View) {
        self.view = view;
        self.trail.clear();
        self.selection = super::Selection::default();
        self.after_navigation();
    }

    fn after_navigation(&mut self) {
        self.layers.retain(|layer| *layer != Layer::Drawer);
        self.scroll = 0;
        self.lock = None;
        self.reset_widths();
        self.invalidate();
    }

    fn step_view(&mut self, step: isize) {
        let views = self.visible_views();
        let at = views.iter().position(|view| *view == self.view);
        let next = at.map_or(0, |at| {
            (at + views.len()).saturating_add_signed(step) % views.len()
        });
        self.switch_view(views[next]);
    }

    fn step_sort(&mut self, step: isize) {
        let visible = self.prefs.visible(self.view, self.tier());
        let current = visible
            .iter()
            .position(|column| *column == self.sort().column)
            .unwrap_or(0);
        let next = (current + visible.len()).saturating_add_signed(step) % visible.len();
        self.set_sort(Sort::for_column(self.view.columns(), visible[next]));
    }

    fn reverse_sort(&mut self) {
        let sort = self.sort();
        self.set_sort(Sort {
            descending: !sort.descending,
            ..sort
        });
    }

    fn sort_by(&mut self, column: usize) {
        if column == self.sort().column {
            self.reverse_sort();
        } else {
            self.set_sort(Sort::for_column(self.view.columns(), column));
        }
    }

    fn set_sort(&mut self, sort: Sort) {
        self.sorts.insert(self.view, sort);
        self.lock = None;
        self.invalidate();
    }

    fn toggle_node(&mut self, open: Option<bool>) {
        let Some(key) = self.selected_key().map(str::to_owned) else {
            return;
        };
        if !self.selected_is_parent() {
            return;
        }
        let groups = self
            .groups
            .entry(self.view)
            .or_insert_with(|| Groups::new(true));
        match open {
            Some(open) => groups.set(&key, open),
            None => groups.toggle(&key),
        }
        self.invalidate();
    }

    fn close_node(&mut self) {
        if self.selected_is_parent() {
            self.toggle_node(Some(false));
            return;
        }
        let parent = self.arranged.as_ref().and_then(|arranged| {
            let index = *arranged.order.get(self.selection.position)?;
            let parent = arranged.table.rows[index].parent?;
            Some(arranged.table.rows[parent].key.clone())
        });
        if let Some(key) = parent {
            let position = self
                .arranged
                .as_ref()
                .and_then(|arranged| arranged.position(&key));
            if let Some(position) = position {
                self.select_at(position);
            }
        }
    }

    fn every_group(&mut self, change: impl FnOnce(&mut Groups)) {
        change(
            self.groups
                .entry(self.view)
                .or_insert_with(|| Groups::new(true)),
        );
        self.invalidate();
    }

    fn toggle_grouping(&mut self) {
        if self.view.grouping().is_none() {
            self.message = Some(format!(
                "{} is already a tree or has nothing to group by: use + and -",
                self.view.title()
            ));
            return;
        }
        if !self.grouped.remove(&self.view) {
            self.grouped.insert(self.view);
        }
        self.selection = super::Selection::default();
        self.invalidate();
    }

    fn drill_target(&self) -> Option<(View, String)> {
        let key = self.selected_key()?;
        if let Some(group) = key.strip_prefix("group:") {
            return Some((View::Conns, format!("listener:{group}")));
        }
        match self.view {
            View::Listeners => Some((View::Conns, format!("listener:{key}"))),
            View::Clients => Some((View::Conns, format!("ip:{key}"))),
            View::Upstreams => Some(match key.split_once('/') {
                Some((_, server)) => (View::Requests, format!("server:{server}")),
                None => (View::Requests, format!("pool:{key}")),
            }),
            _ => None,
        }
    }

    fn drill(&mut self) {
        let Some((view, filter)) = self.drill_target() else {
            self.message = Some("nothing to drill into from this row".into());
            return;
        };
        self.trail.push(super::Crumb {
            view: self.view,
            filter: self.filter.source().to_owned(),
            selected: self.selected_key().map(str::to_owned),
        });
        self.view = view;
        self.set_filter(&filter);
        self.selection = super::Selection::default();
        self.after_navigation();
    }
}
