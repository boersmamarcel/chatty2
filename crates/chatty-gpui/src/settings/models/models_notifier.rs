use gpui::EventEmitter;
use crate::global_entity::GlobalWeakEntity;

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
pub type GlobalModelsNotifier = GlobalWeakEntity<ModelsNotifier>;
