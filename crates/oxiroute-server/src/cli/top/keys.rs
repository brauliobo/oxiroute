use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Key {
    Char(char),
    Up,
    Down,
    PageUp,
    PageDown,
    Home,
    End,
    Tab,
    BackTab,
    Enter,
    Escape,
    Backspace,
    Interrupt,
}

impl Key {
    pub(super) fn from_event(event: KeyEvent) -> Option<Self> {
        let control = event.modifiers.contains(KeyModifiers::CONTROL);
        Some(match event.code {
            KeyCode::Char('c') if control => Self::Interrupt,
            KeyCode::Char(character) if !control => Self::Char(character),
            KeyCode::Up => Self::Up,
            KeyCode::Down => Self::Down,
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
