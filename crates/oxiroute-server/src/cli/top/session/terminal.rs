use std::sync::mpsc::Sender;

use ratatui::crossterm::event;

use super::Message;

pub(super) fn read(messages: &Sender<Message>) {
    while let Ok(event) = event::read() {
        if messages.send(Message::Input(event)).is_err() {
            return;
        }
    }
}
