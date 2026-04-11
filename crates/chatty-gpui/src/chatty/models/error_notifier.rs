use gpui::EventEmitter;
use crate::global_entity::GlobalWeakEntity;

pub struct ErrorNotifier;

impl ErrorNotifier {
    pub fn new() -> Self {
        Self
    }
}

#[derive(Clone, Debug)]
pub enum ErrorNotifierEvent {
    NewError,
}

impl EventEmitter<ErrorNotifierEvent> for ErrorNotifier {}

pub type GlobalErrorNotifier = GlobalWeakEntity<ErrorNotifier>;
