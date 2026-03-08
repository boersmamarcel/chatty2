use gpui::{EventEmitter, Global, WeakEntity};

/// Events related to model loading
#[derive(Clone, Debug)]
pub enum ModelsNotifierEvent {
    /// Emitted when models are initially loaded from disk and providers
    ModelsReady,
}

/// Entity that notifies subscribers when models are ready
pub struct ModelsNotifier;

impl EventEmitter<ModelsNotifierEvent> for ModelsNotifier {}

impl ModelsNotifier {
    pub fn new() -> Self {
        Self
    }
}

/// Global wrapper for the notifier entity
#[derive(Default)]
pub struct GlobalModelsNotifier {
    pub entity: Option<WeakEntity<ModelsNotifier>>,
}

impl Global for GlobalModelsNotifier {}
