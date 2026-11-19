use super::{actions::Action, keys::Key};
use crate::cli::top::prefs::Panel;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Scope {
    Browse,
    Drawer,
    Picker,
    Filter,
    Help,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Binding {
    pub(crate) scope: Scope,
    pub(crate) keys: &'static [Key],
    pub(crate) action: Action,
    pub(crate) help: &'static str,
    pub(crate) chip: Option<(&'static str, &'static str)>,
}

const fn bind(
    scope: Scope,
    keys: &'static [Key],
    action: Action,
    help: &'static str,
    chip: Option<(&'static str, &'static str)>,
) -> Binding {
    Binding {
        scope,
        keys,
        action,
        help,
        chip,
    }
}

const fn c(character: char) -> Key {
    Key::Char(character)
}

use Scope::{Browse, Drawer, Filter, Help as HelpScope, Picker};

pub(crate) const BINDINGS: &[Binding] = &[
    bind(
        Browse,
        &[Key::Function(1), c('?')],
        Action::Help,
        "help",
        Some(("F1", "Help")),
    ),
    bind(
        Browse,
        &[Key::Function(2), c('c')],
        Action::OpenColumns,
        "choose, reorder and hide columns",
        Some(("F2", "Columns")),
    ),
    bind(
        Browse,
        &[Key::Function(3), c('/')],
        Action::OpenFilter,
        "filter rows (words, !not, column:text, ip:PREFIX)",
        Some(("F3", "Filter")),
    ),
    bind(
        Browse,
        &[Key::Function(4), c('V')],
        Action::OpenViews,
        "choose, reorder and hide views",
        Some(("F4", "Views")),
    ),
    bind(
        Browse,
        &[Key::Function(5), c('t')],
        Action::ToggleGrouping,
        "group rows by listener (Clients, Conns, Requests)",
        None,
    ),
    bind(
        Browse,
        &[Key::Function(6), c('>'), c('.')],
        Action::SortNext,
        "sort by the next column",
        Some(("F6", "Sort")),
    ),
    bind(
        Browse,
        &[c('<'), c(',')],
        Action::SortPrevious,
        "sort by the previous column",
        None,
    ),
    bind(
        Browse,
        &[c('r')],
        Action::SortReverse,
        "reverse the sort",
        None,
    ),
    bind(
        Browse,
        &[Key::Function(7), c('h')],
        Action::CollapsePanel(Panel::Meters),
        "collapse or expand the header meters",
        Some(("F7", "Meters")),
    ),
    bind(
        Browse,
        &[c('H')],
        Action::HidePanel(Panel::Meters),
        "hide or restore the header meters",
        None,
    ),
    bind(
        Browse,
        &[c('s')],
        Action::CollapsePanel(Panel::Summary),
        "collapse or expand the summary box",
        None,
    ),
    bind(
        Browse,
        &[c('S')],
        Action::HidePanel(Panel::Summary),
        "hide or restore the summary box",
        None,
    ),
    bind(
        Browse,
        &[c('i')],
        Action::Detail,
        "open the detail drawer of the selected row",
        None,
    ),
    bind(
        Browse,
        &[c('I')],
        Action::HidePanel(Panel::Drawer),
        "hide or restore the detail drawer",
        None,
    ),
    bind(
        Browse,
        &[Key::Function(8), c('d')],
        Action::ToggleDensity,
        "compact density: no header, plain table",
        None,
    ),
    bind(
        Browse,
        &[c('f')],
        Action::ToggleFocus,
        "focus: maximize the table",
        None,
    ),
    bind(
        Browse,
        &[Key::Function(9), c('p')],
        Action::Pause,
        "pause or resume sampling",
        Some(("F9", "Pause")),
    ),
    bind(
        Browse,
        &[c('w')],
        Action::CycleWindow,
        "rate window 2s, 10s, 60s, life",
        Some(("w", "Window")),
    ),
    bind(
        Browse,
        &[c('u')],
        Action::ToggleUnits,
        "bytes or bits",
        None,
    ),
    bind(
        Browse,
        &[c('o')],
        Action::LockOrder,
        "freeze or release the row order",
        None,
    ),
    bind(
        Browse,
        &[c('m')],
        Action::ToggleMouse,
        "mouse capture on or off",
        None,
    ),
    bind(
        Browse,
        &[c('T')],
        Action::CycleTheme,
        "cycle the theme",
        None,
    ),
    bind(
        Browse,
        &[Key::Tab, c(']')],
        Action::NextView,
        "next view",
        None,
    ),
    bind(
        Browse,
        &[Key::BackTab, c('[')],
        Action::PreviousView,
        "previous view",
        None,
    ),
    bind(Browse, &[Key::Up, c('k')], Action::Up, "move up", None),
    bind(
        Browse,
        &[Key::Down, c('j')],
        Action::Down,
        "move down",
        None,
    ),
    bind(Browse, &[Key::PageUp], Action::PageUp, "page up", None),
    bind(
        Browse,
        &[Key::PageDown],
        Action::PageDown,
        "page down",
        None,
    ),
    bind(Browse, &[Key::Home, c('g')], Action::Top, "first row", None),
    bind(
        Browse,
        &[Key::End, c('G')],
        Action::Bottom,
        "last row",
        None,
    ),
    bind(
        Browse,
        &[Key::Enter],
        Action::Activate,
        "open or close a group, else open the detail drawer",
        Some(("Enter", "Detail")),
    ),
    bind(
        Browse,
        &[c(' ')],
        Action::ToggleNode,
        "open or close the selected group",
        None,
    ),
    bind(
        Browse,
        &[Key::Right],
        Action::OpenNode,
        "open the selected group",
        None,
    ),
    bind(
        Browse,
        &[Key::Left],
        Action::CloseNode,
        "close the group, or go to its parent",
        None,
    ),
    bind(
        Browse,
        &[c('+')],
        Action::ExpandAll,
        "open every group",
        None,
    ),
    bind(
        Browse,
        &[c('-')],
        Action::CollapseAll,
        "close every group",
        None,
    ),
    bind(
        Browse,
        &[Key::Function(10), c('q')],
        Action::Quit,
        "quit",
        Some(("F10", "Quit")),
    ),
    bind(
        Drawer,
        &[Key::Enter],
        Action::Drill,
        "jump to the connections or requests of this row",
        Some(("Enter", "Drill down")),
    ),
    bind(
        Drawer,
        &[Key::Escape],
        Action::Pop,
        "close the drawer",
        Some(("Esc", "Close")),
    ),
    bind(
        Picker,
        &[Key::Up, c('k')],
        Action::PickerUp,
        "move up",
        Some(("Up/Down", "Move")),
    ),
    bind(
        Picker,
        &[Key::Down, c('j')],
        Action::PickerDown,
        "move down",
        None,
    ),
    bind(
        Picker,
        &[c(' '), Key::Enter],
        Action::PickerToggle,
        "show or hide",
        Some(("Space", "Toggle")),
    ),
    bind(
        Picker,
        &[c('K')],
        Action::PickerMoveUp,
        "move the item up",
        Some(("K/J", "Reorder")),
    ),
    bind(
        Picker,
        &[c('J')],
        Action::PickerMoveDown,
        "move the item down",
        None,
    ),
    bind(
        Picker,
        &[c('r')],
        Action::PickerReset,
        "restore the defaults",
        Some(("r", "Reset")),
    ),
    bind(
        Picker,
        &[Key::Escape, c('q')],
        Action::Pop,
        "close",
        Some(("Esc", "Close")),
    ),
    bind(
        Filter,
        &[Key::Enter],
        Action::FilterEnter,
        "keep the filter",
        Some(("Enter", "Keep")),
    ),
    bind(
        Filter,
        &[Key::Escape],
        Action::FilterClear,
        "clear the filter",
        Some(("Esc", "Clear")),
    ),
    bind(
        Filter,
        &[Key::Backspace],
        Action::FilterErase,
        "erase a character",
        None,
    ),
    bind(
        Filter,
        &[Key::Ctrl('u')],
        Action::FilterWipe,
        "clear the text",
        None,
    ),
    bind(
        HelpScope,
        &[Key::Escape, c('q'), c('?'), Key::Function(1), Key::Enter],
        Action::Pop,
        "close help",
        Some(("Esc", "Close")),
    ),
    bind(
        HelpScope,
        &[Key::Up, c('k')],
        Action::PickerUp,
        "scroll up",
        Some(("Up/Down", "Scroll")),
    ),
    bind(
        HelpScope,
        &[Key::Down, c('j'), Key::PageDown],
        Action::PickerDown,
        "scroll down",
        None,
    ),
];

pub(crate) fn lookup(scope: Scope, key: Key) -> Option<Action> {
    if let (Scope::Browse | Scope::Drawer, Key::Char(digit @ '1'..='9')) = (scope, key) {
        return digit
            .to_digit(10)
            .and_then(|number| usize::try_from(number - 1).ok())
            .map(Action::Slot);
    }
    let scopes: &[Scope] = if scope == Scope::Drawer {
        &[Drawer, Browse]
    } else {
        &[scope]
    };
    scopes
        .iter()
        .find_map(|scope| {
            BINDINGS
                .iter()
                .find(|binding| binding.scope == *scope && binding.keys.contains(&key))
        })
        .map(|binding| binding.action)
}

pub(crate) fn chips(scope: Scope) -> Vec<(&'static str, &'static str, Action)> {
    let scopes: &[Scope] = if scope == Scope::Drawer {
        &[Drawer, Browse]
    } else {
        &[scope]
    };
    scopes
        .iter()
        .flat_map(|scope| {
            BINDINGS
                .iter()
                .filter(move |binding| binding.scope == *scope)
        })
        .filter_map(|binding| binding.chip.map(|(key, name)| (key, name, binding.action)))
        .collect()
}

pub(crate) fn label(key: Key) -> String {
    match key {
        Key::Char(' ') => "Space".into(),
        Key::Char(character) => character.to_string(),
        Key::Function(number) => format!("F{number}"),
        Key::Ctrl(character) => format!("Ctrl-{}", character.to_ascii_uppercase()),
        Key::Up => "Up".into(),
        Key::Down => "Down".into(),
        Key::Left => "Left".into(),
        Key::Right => "Right".into(),
        Key::PageUp => "PgUp".into(),
        Key::PageDown => "PgDn".into(),
        Key::Home => "Home".into(),
        Key::End => "End".into(),
        Key::Tab => "Tab".into(),
        Key::BackTab => "S-Tab".into(),
        Key::Enter => "Enter".into(),
        Key::Escape => "Esc".into(),
        Key::Backspace => "Bksp".into(),
    }
}

pub(crate) fn help_lines() -> Vec<(String, &'static str)> {
    BINDINGS
        .iter()
        .filter(|binding| matches!(binding.scope, Browse | Drawer))
        .map(|binding| {
            let keys = binding
                .keys
                .iter()
                .map(|key| label(*key))
                .collect::<Vec<_>>()
                .join(" ");
            (keys, binding.help)
        })
        .collect()
}
