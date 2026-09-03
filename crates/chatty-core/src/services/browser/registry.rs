//! Per-conversation lookup for the live [`BrowserManager`] a running agent
//! turn built (AGE-155).
//!
//! The artifact window needs a way to reach the browser session an agent is
//! driving so it can screencast it, but nothing before this connected chatty-core's
//! agent build path to chatty-gpui's UI layer. This is that connection point:
//! `agent_factory` registers the manager it built for a conversation here, and
//! the UI looks it up by the same id. Nothing here owns process lifecycle —
//! that stays with whatever already holds the `Arc` (the agent's tool registry).

use std::collections::HashMap;
use std::sync::{Arc, LazyLock};

use parking_lot::Mutex;

use super::BrowserManager;

static REGISTRY: LazyLock<Mutex<HashMap<String, Arc<BrowserManager>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Record the manager a running agent turn built for `conversation_id`,
/// replacing whatever was registered before it.
pub fn register(conversation_id: impl Into<String>, manager: Arc<BrowserManager>) {
    REGISTRY.lock().insert(conversation_id.into(), manager);
}

/// The manager currently registered for `conversation_id`, if any.
pub fn for_conversation(conversation_id: &str) -> Option<Arc<BrowserManager>> {
    REGISTRY.lock().get(conversation_id).cloned()
}

/// Drop the registration for a conversation — called when it's deleted.
pub fn unregister(conversation_id: &str) {
    REGISTRY.lock().remove(conversation_id);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_then_lookup_then_unregister() {
        let id = format!("test-conv-{}", uuid::Uuid::new_v4());
        assert!(for_conversation(&id).is_none());

        let manager = Arc::new(BrowserManager::lane_a(None));
        register(id.clone(), manager.clone());
        assert!(Arc::ptr_eq(&for_conversation(&id).unwrap(), &manager));

        // Registering again for the same id replaces, not accumulates.
        let manager2 = Arc::new(BrowserManager::lane_a(None));
        register(id.clone(), manager2.clone());
        assert!(Arc::ptr_eq(&for_conversation(&id).unwrap(), &manager2));

        unregister(&id);
        assert!(for_conversation(&id).is_none());
    }
}
