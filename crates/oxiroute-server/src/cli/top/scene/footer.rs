use ratatui::layout::Rect;

use crate::cli::top::{
    app::{Action, App, Layer, Scope, chips},
    layout::Layout,
    prefs::Panel,
};

#[derive(Clone, Debug)]
pub(crate) struct ChipSlot {
    pub(crate) rect: Rect,
    pub(crate) key: String,
    pub(crate) name: String,
    pub(crate) action: Action,
    pub(crate) restore: bool,
}

fn width(text: &str) -> u16 {
    u16::try_from(text.chars().count()).unwrap_or(u16::MAX)
}

fn chip_width(key: &str, name: &str) -> u16 {
    width(key) + width(name) + 2
}

fn renamed(app: &App, name: &str, action: Action) -> String {
    match action {
        Action::Pause if app.switches.paused => "Resume".into(),
        Action::CycleWindow => format!("Rates {}", app.window.label()),
        _ => name.to_owned(),
    }
}

fn restore_key(panel: Panel) -> String {
    panel.key().to_ascii_uppercase().to_string()
}

pub(super) fn build(app: &App, layout: &Layout) -> Vec<ChipSlot> {
    let scope = app.layers.last().map_or(Scope::Browse, Layer::scope);
    let mut regular: Vec<(String, String, Action)> = chips(scope)
        .into_iter()
        .map(|(key, name, action)| (key.to_owned(), renamed(app, name, action), action))
        .collect();
    let restores: Vec<(String, String, Action)> = if scope == Scope::Browse {
        app.prefs
            .panels
            .hidden()
            .into_iter()
            .map(|panel| {
                (
                    restore_key(panel),
                    panel.title().to_owned(),
                    Action::RestorePanel(panel),
                )
            })
            .collect()
    } else {
        Vec::new()
    };
    let area = layout.keys;
    let restore_width: u16 = restores
        .iter()
        .map(|(key, name, _)| chip_width(key, name))
        .sum::<u16>()
        + if restores.is_empty() { 0 } else { 8 };
    let room = area.width.saturating_sub(restore_width);
    while regular.len() > 2
        && regular
            .iter()
            .map(|(key, name, _)| chip_width(key, name))
            .sum::<u16>()
            > room
    {
        regular.remove(regular.len() - 2);
    }
    let mut slots = Vec::new();
    let mut x = area.x;
    for (key, name, action) in regular {
        let chip = chip_width(&key, &name);
        slots.push(ChipSlot {
            rect: Rect::new(x, area.y, chip.saturating_sub(1), 1),
            key,
            name,
            action,
            restore: false,
        });
        x += chip;
    }
    let mut x = (area.x + area.width).saturating_sub(restore_width)
        + if restores.is_empty() { 0 } else { 8 };
    for (key, name, action) in restores {
        let chip = chip_width(&key, &name);
        slots.push(ChipSlot {
            rect: Rect::new(x, area.y, chip.saturating_sub(1), 1),
            key,
            name,
            action,
            restore: true,
        });
        x += chip;
    }
    slots
}
