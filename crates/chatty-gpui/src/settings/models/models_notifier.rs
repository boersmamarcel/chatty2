use crate::global_entity::GlobalStrongEntity;
use gpui::EventEmitter;

/// Events related to model loading and mutation
#[derive(Clone, Debug)]
pub enum ModelsNotifierEvent {
    /// Emitted when models are initially loaded from disk and providers
    ModelsReady,
    /// Emitted when models are added, updated, removed, or synced
    ModelsChanged,
}

/// Entity that notifies subscribers when models are ready or change
pub struct ModelsNotifier;

impl EventEmitter<ModelsNotifierEvent> for ModelsNotifier {}

impl ModelsNotifier {
    pub fn new() -> Self {
        Self
    }
}

/// Global wrapper — strong so the notifier stays alive for the app lifetime.
pub type GlobalModelsNotifier = GlobalStrongEntity<ModelsNotifier>;
