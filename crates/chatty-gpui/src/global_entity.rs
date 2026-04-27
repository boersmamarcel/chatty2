//! Generic global entity wrappers for GPUI's type-map based global state.
//!
//! Replaces bespoke `GlobalFoo { entity: Option<WeakEntity<Foo>> }` structs
//! with two reusable generics: [`GlobalWeakEntity<T>`] and [`GlobalStrongEntity<T>`].
//!
//! # Usage
//!
//! ```ignore
//! // Define a type alias (replaces a bespoke struct + impl Global):
//! pub type GlobalMyNotifier = GlobalWeakEntity<MyNotifier>;
//!
//! // Set:
//! cx.set_global(GlobalMyNotifier::new(entity.downgrade()));
//!
//! // Read + upgrade in one step:
//! if let Some(notifier) = cx
//!     .try_global::<GlobalMyNotifier>()
//!     .and_then(|g| g.try_upgrade())
//! {
//!     notifier.update(cx, |_, cx| cx.emit(MyEvent));
//! }
//! ```

use gpui::{Entity, Global, WeakEntity};

/// Global wrapper holding a **weak** entity reference.
///
/// Use when the entity's lifetime is managed elsewhere (e.g., notifiers
/// owned by a parent entity). The caller must `try_upgrade()` before use.
pub struct GlobalWeakEntity<T> {
    pub entity: Option<WeakEntity<T>>,
}

impl<T: 'static> GlobalWeakEntity<T> {
    pub fn new(entity: WeakEntity<T>) -> Self {
        Self {
            entity: Some(entity),
        }
    }

    /// Try to upgrade the weak reference to a strong one.
    /// Returns `None` if the entity has been dropped or no entity was set.
    pub fn try_upgrade(&self) -> Option<Entity<T>> {
        self.entity.as_ref().and_then(|w| w.upgrade())
    }
}

impl<T: 'static> Default for GlobalWeakEntity<T> {
    fn default() -> Self {
        Self { entity: None }
    }
}

impl<T: 'static> Global for GlobalWeakEntity<T> {}

/// Global wrapper holding a **strong** entity reference.
///
/// Use when the global must keep the entity alive (e.g., StreamManager).
pub struct GlobalStrongEntity<T> {
    pub entity: Option<Entity<T>>,
}

impl<T: 'static> GlobalStrongEntity<T> {
    pub fn new(entity: Entity<T>) -> Self {
        Self {
            entity: Some(entity),
        }
    }

    /// Get a clone of the strong entity reference.
    pub fn get(&self) -> Option<Entity<T>> {
        self.entity.clone()
    }
}

impl<T: 'static> Default for GlobalStrongEntity<T> {
    fn default() -> Self {
        Self { entity: None }
    }
}

impl<T: 'static> Global for GlobalStrongEntity<T> {}
