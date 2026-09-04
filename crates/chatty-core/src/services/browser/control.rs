//! The control lock (AGE-156): who is driving the browser right now.
//!
//! Arbitration rules, straight from the ticket — enforced here, not left to
//! callers to remember:
//!
//! - The agent holds control by default. It exists to work; it should not
//!   have to ask.
//! - The user *takes* control, never requests it — [`take`] always
//!   succeeds immediately, there is no negotiation.
//! - While the user holds it, mutating session actions (navigate, resize)
//!   are refused with [`super::error::BrowserError::ControlHeldByUser`].
//!   Read-only tools (snapshot, screenshot, console, network) never call
//!   [`ControlLock::ensure_agent`] and keep working — watching never
//!   collides.
//! - Handing control back invalidates every outstanding element ref — the
//!   page moved underneath whatever the agent last looked at. The caller
//!   ([`super::session::BrowserSession::release_control`]) is responsible
//!   for bumping the snapshot generation; this type only tracks the holder.

use parking_lot::Mutex;
use tracing::info;

/// Who is driving the browser.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ControlHolder {
    Agent,
    User,
}

use super::error::BrowserError;

pub(super) struct ControlLock {
    holder: Mutex<ControlHolder>,
}

impl ControlLock {
    pub(super) fn new() -> Self {
        Self {
            holder: Mutex::new(ControlHolder::Agent),
        }
    }

    pub(super) fn holder(&self) -> ControlHolder {
        *self.holder.lock()
    }

    /// The user takes control. Returns the holder *before* the transition,
    /// so the caller can tell a genuine handover from a no-op.
    pub(super) fn take(&self) -> ControlHolder {
        let mut holder = self.holder.lock();
        let previous = *holder;
        *holder = ControlHolder::User;
        if previous != ControlHolder::User {
            info!("browser: user took control");
        }
        previous
    }

    /// Hand control back to the agent. Returns the holder *before* the
    /// transition.
    pub(super) fn release(&self) -> ControlHolder {
        let mut holder = self.holder.lock();
        let previous = *holder;
        *holder = ControlHolder::Agent;
        if previous != ControlHolder::Agent {
            info!("browser: control released back to the agent");
        }
        previous
    }

    /// Err if the user currently holds control. Call this at the top of
    /// every mutating session action.
    pub(super) fn ensure_agent(&self) -> Result<(), BrowserError> {
        if self.holder() == ControlHolder::User {
            Err(BrowserError::ControlHeldByUser)
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_holds_control_by_default() {
        let lock = ControlLock::new();
        assert_eq!(lock.holder(), ControlHolder::Agent);
        assert!(lock.ensure_agent().is_ok());
    }

    #[test]
    fn taking_control_refuses_agent_actions_until_released() {
        let lock = ControlLock::new();
        let previous = lock.take();
        assert_eq!(previous, ControlHolder::Agent);
        assert_eq!(lock.holder(), ControlHolder::User);
        assert!(matches!(
            lock.ensure_agent(),
            Err(BrowserError::ControlHeldByUser)
        ));

        let previous = lock.release();
        assert_eq!(previous, ControlHolder::User);
        assert_eq!(lock.holder(), ControlHolder::Agent);
        assert!(lock.ensure_agent().is_ok());
    }

    #[test]
    fn taking_control_twice_is_idempotent() {
        let lock = ControlLock::new();
        lock.take();
        let previous = lock.take();
        assert_eq!(previous, ControlHolder::User);
        assert_eq!(lock.holder(), ControlHolder::User);
    }
}
