use gpui::{EventEmitter, Global, WeakEntity};

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

pub struct GlobalErrorNotifier {
    pub entity: Option<WeakEntity<ErrorNotifier>>,
}

impl Global for GlobalErrorNotifier {}

impl Default for GlobalErrorNotifier {
    fn default() -> Self {
        Self { entity: None }
    }
}
