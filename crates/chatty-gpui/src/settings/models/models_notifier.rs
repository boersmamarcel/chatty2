use crate::global_entity::GlobalStrongEntity;
use gpui::{App, EventEmitter};
use tracing::warn;

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
///
/// Created in `main.rs` before `ChattyApp`, so it cannot be kept alive via a
/// `ChattyApp` field the way `AgentConfigNotifier` is. Same pattern as
/// `GlobalStreamManager`.
pub type GlobalModelsNotifier = GlobalStrongEntity<ModelsNotifier>;

/// Emit `ModelsChanged` so the main window chat-input model picker refreshes.
pub fn emit_models_changed(cx: &mut App) {
    if let Some(notifier) = cx
        .try_global::<GlobalModelsNotifier>()
        .and_then(|g| g.get())
    {
        notifier.update(cx, |_notifier, cx| {
            cx.emit(ModelsNotifierEvent::ModelsChanged);
        });
    } else {
        warn!("emit_models_changed: GlobalModelsNotifier not found — chat input will not refresh");
    }
}
