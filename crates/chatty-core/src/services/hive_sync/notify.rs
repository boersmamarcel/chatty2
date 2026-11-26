//! Deciding when a cloud conversation is worth an OS notification (HX-12).
//!
//! Pure: the pull's rows go in, notices come out. Showing them is the
//! frontend's. Derived from the sync pull's `turn_status`, so no server API
//! of its own.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use super::wire::TurnStatus;

/// What a notification says happened.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CloudNotice {
    /// The conversation's turn finished.
    Finished { local_id: String, title: String },
    /// The conversation's turn needs an answer from the user.
    WaitingForYou { local_id: String, title: String },
}

impl CloudNotice {
    pub fn local_id(&self) -> &str {
        match self {
            CloudNotice::Finished { local_id, .. }
            | CloudNotice::WaitingForYou { local_id, .. } => local_id,
        }
    }

    /// The notification's one line.
    pub fn text(&self) -> String {
        match self {
            CloudNotice::Finished { title, .. } => format!("Finished: {title}"),
            CloudNotice::WaitingForYou { title, .. } => format!("{title} is waiting for you"),
        }
    }
}

/// Remembers each conversation's last seen status, so only a *change* speaks.
#[derive(Debug, Default)]
pub struct TransitionDetector {
    last: HashMap<String, TurnStatus>,
}

impl TransitionDetector {
    /// Record one row of a pull and say whether it is news.
    ///
    /// Only a conversation `engaged` with — one this device opened or sent
    /// to — can notify, so work that exists only on the web stays quiet. A
    /// conversation seen for the first time never notifies: there is nothing
    /// it changed from. `Unknown` counts as idle.
    pub fn observe(
        &mut self,
        local_id: &str,
        title: &str,
        status: TurnStatus,
        engaged: bool,
    ) -> Option<CloudNotice> {
        let status = if status == TurnStatus::Unknown {
            TurnStatus::Idle
        } else {
            status
        };
        let before = self.last.insert(local_id.to_string(), status)?;
        if !engaged {
            return None;
        }
        let local_id = local_id.to_string();
        let title = title.to_string();
        match (before, status) {
            (TurnStatus::Running | TurnStatus::WaitingForYou, TurnStatus::Idle) => {
                Some(CloudNotice::Finished { local_id, title })
            }
            (before, TurnStatus::WaitingForYou) if before != TurnStatus::WaitingForYou => {
                Some(CloudNotice::WaitingForYou { local_id, title })
            }
            _ => None,
        }
    }

    /// The conversation is gone (deleted): forget it.
    pub fn forget(&mut self, local_id: &str) {
        self.last.remove(local_id);
    }

    /// A detector that remembers `last`, as [`NotifyMemory`] kept it.
    pub fn with_last(last: HashMap<String, TurnStatus>) -> Self {
        Self { last }
    }

    /// Each conversation's last seen status.
    pub fn last(&self) -> &HashMap<String, TurnStatus> {
        &self.last
    }
}

/// What notifying needs to survive a restart (AGE-2127): each conversation's
/// last seen status and which ones this device engaged with. Kept in memory
/// only, a turn that finished while the app was closed was never news: the
/// first pull after the start saw it idle with nothing to compare.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct NotifyMemory {
    #[serde(default)]
    pub last: HashMap<String, TurnStatus>,
    #[serde(default)]
    pub engaged: HashSet<String>,
}

impl NotifyMemory {
    /// Where the desktop keeps it: `cloud_notify.json` in chatty's config
    /// directory.
    pub fn default_path() -> Option<PathBuf> {
        dirs::config_dir().map(|dir| dir.join("chatty").join("cloud_notify.json"))
    }

    /// The memory at `path`; empty when there is none or it does not read.
    pub fn load(path: &Path) -> Self {
        std::fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    /// Write the memory to `path`, replacing it whole.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec(self)?)?;
        std::fs::rename(tmp, path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn observe(
        d: &mut TransitionDetector,
        id: &str,
        s: TurnStatus,
        engaged: bool,
    ) -> Option<String> {
        d.observe(id, "Trip plan", s, engaged).map(|n| n.text())
    }

    #[test]
    fn running_to_idle_notifies_once() {
        let mut d = TransitionDetector::default();
        assert_eq!(observe(&mut d, "a", TurnStatus::Running, true), None);
        assert_eq!(
            observe(&mut d, "a", TurnStatus::Idle, true).as_deref(),
            Some("Finished: Trip plan")
        );
        // The next pull sees it idle again: nothing new.
        assert_eq!(observe(&mut d, "a", TurnStatus::Idle, true), None);
    }

    #[test]
    fn idle_to_idle_never_notifies() {
        let mut d = TransitionDetector::default();
        for _ in 0..3 {
            assert_eq!(observe(&mut d, "a", TurnStatus::Idle, true), None);
        }
    }

    #[test]
    fn waiting_for_you_notifies_when_it_starts_waiting_not_while_it_waits() {
        let mut d = TransitionDetector::default();
        observe(&mut d, "a", TurnStatus::Running, true);
        assert_eq!(
            observe(&mut d, "a", TurnStatus::WaitingForYou, true).as_deref(),
            Some("Trip plan is waiting for you")
        );
        assert_eq!(observe(&mut d, "a", TurnStatus::WaitingForYou, true), None);
        // Answered, the turn runs on and then finishes.
        assert_eq!(observe(&mut d, "a", TurnStatus::Running, true), None);
        assert!(observe(&mut d, "a", TurnStatus::Idle, true).is_some());
    }

    #[test]
    fn a_conversation_this_device_never_opened_never_notifies() {
        let mut d = TransitionDetector::default();
        observe(&mut d, "web-only", TurnStatus::Running, false);
        assert_eq!(observe(&mut d, "web-only", TurnStatus::Idle, false), None);
        // …and engaging later does not replay what already happened.
        assert_eq!(observe(&mut d, "web-only", TurnStatus::Idle, true), None);
    }

    #[test]
    fn another_conversations_transition_is_not_this_ones() {
        let mut d = TransitionDetector::default();
        observe(&mut d, "a", TurnStatus::Running, true);
        observe(&mut d, "b", TurnStatus::Idle, true);
        assert_eq!(observe(&mut d, "b", TurnStatus::Idle, true), None);
        assert!(observe(&mut d, "a", TurnStatus::Idle, true).is_some());
    }

    #[test]
    fn the_first_sighting_is_not_a_change() {
        let mut d = TransitionDetector::default();
        assert_eq!(observe(&mut d, "a", TurnStatus::WaitingForYou, true), None);
        assert_eq!(
            observe(&mut d, "a", TurnStatus::Unknown, true).as_deref(),
            Some("Finished: Trip plan")
        );
    }

    /// AGE-2127: a turn that was running when the app closed and finished
    /// before it opened again notifies on the first pull after the start.
    #[test]
    fn a_finish_while_closed_notifies_after_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cloud_notify.json");

        let mut d = TransitionDetector::default();
        observe(&mut d, "a", TurnStatus::Running, true);
        NotifyMemory {
            last: d.last().clone(),
            engaged: ["a".to_string()].into(),
        }
        .save(&path)
        .unwrap();

        // The app starts again.
        let memory = NotifyMemory::load(&path);
        let mut d = TransitionDetector::with_last(memory.last);
        let engaged = memory.engaged.contains("a");
        assert_eq!(
            observe(&mut d, "a", TurnStatus::Idle, engaged).as_deref(),
            Some("Finished: Trip plan")
        );
        assert_eq!(
            NotifyMemory::load(&dir.path().join("none.json")),
            NotifyMemory::default()
        );
    }
}
