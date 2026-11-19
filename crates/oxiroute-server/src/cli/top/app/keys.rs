use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Key {
    Char(char),
    Function(u8),
    Ctrl(char),
    Up,
    Down,
    Left,
    Right,
    PageUp,
    PageDown,
    Home,
    End,
    Tab,
    BackTab,
    Enter,
    Escape,
    Backspace,
}

impl Key {
    pub(crate) fn from_event(event: KeyEvent) -> Option<Self> {
        let control = event.modifiers.contains(KeyModifiers::CONTROL);
        Some(match event.code {
            KeyCode::Char(character) if control => Self::Ctrl(character.to_ascii_lowercase()),
            KeyCode::Char(character) => Self::Char(character),
            KeyCode::F(number) => Self::Function(number),
            KeyCode::Up => Self::Up,
            KeyCode::Down => Self::Down,
            KeyCode::Left => Self::Left,
            KeyCode::Right => Self::Right,
            KeyCode::PageUp => Self::PageUp,
            KeyCode::PageDown => Self::PageDown,
            KeyCode::Home => Self::Home,
            KeyCode::End => Self::End,
            KeyCode::Tab => Self::Tab,
            KeyCode::BackTab => Self::BackTab,
            KeyCode::Enter => Self::Enter,
            KeyCode::Esc => Self::Escape,
            KeyCode::Backspace => Self::Backspace,
            _ => return None,
        })
    }
}
