//! [`EmbeddedTerminals`]: the dock's terminals as the agent sees them
//! (AGE-583), a [`TerminalSource`] for `terminal_read`.
//!
//! The dock registers each tab it opens and removes it on close; the agent's
//! tool lists and reads through the same `Arc` on its own runtime. Nothing
//! here touches a gpui entity: an entry holds a [`Weak`] of the tab's
//! [`TerminalHandle`], which is thread-safe, so a read snapshots the grid
//! under its own lock without the main thread. A closed tab's shell dies
//! with its view; the weak reference only lets a read in flight finish.
//!
//! Human tabs start unshared ([`TerminalAccess::None`]): they are listed
//! (id, title, directory, access) but never read. The Agent tab (T8b,
//! AGE-586) registers with [`TerminalKind::Agent`] and is always readable.
//! Every read, the agent's own tab included, is refused while the terminal
//! is at a hidden-input prompt ([`TerminalHandle::input_hidden`]).

use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, Weak};

use async_trait::async_trait;
use chatty_core::services::terminal::{
    HIDDEN_INPUT_MESSAGE, Region, TerminalAccess, TerminalBackend, TerminalInfo, TerminalKind,
    TerminalSource, TerminalText,
};
use chatty_core::settings::models::general_model::TerminalShareDefault;
use chatty_terminal::TerminalHandle;
use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};

use super::dock::tab_title;

/// The window's embedded terminals, shared between the dock (which keeps it
/// current) and the agent's `terminal_read` (which reads it).
#[derive(Default)]
pub struct EmbeddedTerminals {
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    entries: Vec<Entry>,
    next_id: u64,
    /// Bumped on every focus, so the most recently focused tab sorts first.
    clock: u64,
    /// Told the id of every terminal the agent reads, for the dock's flash.
    read_listeners: Vec<UnboundedSender<String>>,
}

struct Entry {
    id: String,
    handle: Weak<TerminalHandle>,
    kind: TerminalKind,
    access: TerminalAccess,
    /// For the title: the shell's name and where it started.
    shell_name: String,
    start_cwd: PathBuf,
    focused_at: u64,
}

/// The process-wide registry, as a gpui global.
#[derive(Default)]
struct EmbeddedTerminalsGlobal(Arc<EmbeddedTerminals>);

impl gpui::Global for EmbeddedTerminalsGlobal {}

impl EmbeddedTerminals {
    /// The app's registry, created on first use. One per process: the app
    /// has one main window, and ids stay unique if it ever has more.
    pub fn global(cx: &mut gpui::App) -> Arc<Self> {
        cx.default_global::<EmbeddedTerminalsGlobal>().0.clone()
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Add a terminal and return its id (`term-1`, …). A human tab starts
    /// unshared; the agent's own is readable.
    pub fn register(
        &self,
        handle: &Arc<TerminalHandle>,
        kind: TerminalKind,
        shell_name: String,
        start_cwd: PathBuf,
    ) -> String {
        let mut inner = self.lock();
        inner.next_id += 1;
        inner.clock += 1;
        let id = format!("term-{}", inner.next_id);
        let (focused_at, access) = (
            inner.clock,
            match kind {
                TerminalKind::Agent => TerminalAccess::Read,
                _ => TerminalAccess::None,
            },
        );
        inner.entries.push(Entry {
            id: id.clone(),
            handle: Arc::downgrade(handle),
            kind,
            access,
            shell_name,
            start_cwd,
            focused_at,
        });
        id
    }

    /// Forget a terminal (its tab closed).
    pub fn remove(&self, id: &str) {
        self.lock().entries.retain(|e| e.id != id);
    }

    /// The access the human gave `id`; `None` for an unknown id.
    pub fn access(&self, id: &str) -> TerminalAccess {
        self.lock()
            .entries
            .iter()
            .find(|e| e.id == id)
            .map_or(TerminalAccess::None, |e| e.access)
    }

    /// Share (or unshare, with [`TerminalAccess::None`]) a human tab. The
    /// agent's own tab is not the human's to share and is left as it is.
    pub fn set_access(&self, id: &str, access: TerminalAccess) {
        if let Some(entry) = self
            .lock()
            .entries
            .iter_mut()
            .find(|e| e.id == id && e.kind == TerminalKind::Human)
        {
            entry.access = access;
        }
    }

    /// The human focused `id`: it becomes the default target, if shared.
    pub fn focused(&self, id: &str) {
        let mut inner = self.lock();
        inner.clock += 1;
        let now = inner.clock;
        if let Some(entry) = inner.entries.iter_mut().find(|e| e.id == id) {
            entry.focused_at = now;
        }
    }

    /// Ids of the terminals the agent reads, as they are read.
    pub fn subscribe_reads(&self) -> UnboundedReceiver<String> {
        let (tx, rx) = unbounded();
        self.lock().read_listeners.push(tx);
        rx
    }
}

#[async_trait]
impl TerminalSource for EmbeddedTerminals {
    /// Every open tab, shared or not, most recently focused first.
    async fn list(&self) -> Vec<TerminalInfo> {
        // Upgrade under the lock, title outside it: a tab closed meanwhile
        // may drop its last handle here, and that joins the PTY thread.
        let mut open: Vec<_> = {
            let mut inner = self.lock();
            inner.entries.retain(|e| e.handle.strong_count() > 0);
            inner
                .entries
                .iter()
                .filter_map(|e| {
                    let handle = e.handle.upgrade()?;
                    Some((
                        e.focused_at,
                        handle,
                        e.id.clone(),
                        e.kind,
                        e.access,
                        e.shell_name.clone(),
                        e.start_cwd.clone(),
                    ))
                })
                .collect()
        };
        open.sort_by_key(|entry| std::cmp::Reverse(entry.0));
        open.into_iter()
            .map(|(_, handle, id, kind, access, shell_name, start_cwd)| {
                let cwd = handle.current_dir().unwrap_or(start_cwd);
                TerminalInfo {
                    id,
                    title: tab_title(
                        handle.foreground_process_name().as_deref(),
                        &shell_name,
                        &cwd,
                    ),
                    cwd: (!cwd.as_os_str().is_empty()).then(|| cwd.display().to_string()),
                    backend: TerminalBackend::Embedded,
                    kind,
                    access,
                }
            })
            .collect()
    }

    async fn read(&self, id: &str, region: Region) -> anyhow::Result<TerminalText> {
        let (handle, access) = {
            let inner = self.lock();
            let entry = inner
                .entries
                .iter()
                .find(|e| e.id == id)
                .ok_or_else(|| anyhow::anyhow!("no terminal `{id}`: its tab may be closed"))?;
            (entry.handle.upgrade(), entry.access)
        };
        // `terminal_read` checks access first; this is the backstop.
        if !access.can_read() {
            anyhow::bail!("terminal `{id}` is not shared with you");
        }
        let handle = handle.ok_or_else(|| anyhow::anyhow!("terminal `{id}` was closed"))?;
        if handle.input_hidden() == Some(true) {
            anyhow::bail!(HIDDEN_INPUT_MESSAGE);
        }
        let region = match region {
            Region::Screen => chatty_terminal::Region::Screen,
            Region::Scrollback { lines } => chatty_terminal::Region::Scrollback { lines },
            other => anyhow::bail!("reading {other:?} is not supported for an embedded terminal"),
        };
        let snap = handle.snapshot(region);
        self.lock()
            .read_listeners
            .retain(|tx| tx.unbounded_send(id.to_string()).is_ok());
        Ok(TerminalText {
            text: snap.text,
            cursor_row: snap.cursor_row,
            cols: snap.cols,
            rows: snap.rows,
        })
    }
}

/// The registry as `terminal_read`'s source, for an agent built on `cx`
/// (AGE-583): every desktop agent gets it, so the tool is there when the
/// human shares a tab after the agent was built.
pub fn embedded_terminal_source(cx: &gpui::AsyncApp) -> Option<Arc<dyn TerminalSource>> {
    cx.update(|cx| EmbeddedTerminals::global(cx) as Arc<dyn TerminalSource>)
        .map_err(|e| tracing::warn!(error = ?e, "Failed to read the embedded terminals"))
        .ok()
}

/// What a click on a tab's eye icon does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShareClick {
    /// The tab is shared: stop sharing it, no questions.
    Unshare,
    /// Share at this level without asking (a remembered choice).
    Share(TerminalAccess),
    /// Open the share dialog.
    Ask,
}

/// The eye icon's action for a tab at `current` access under the "When
/// sharing a terminal" setting.
pub fn share_click(current: TerminalAccess, remembered: TerminalShareDefault) -> ShareClick {
    if current.can_read() {
        return ShareClick::Unshare;
    }
    match remembered {
        TerminalShareDefault::Ask => ShareClick::Ask,
        TerminalShareDefault::ReadOnly => ShareClick::Share(TerminalAccess::Read),
        TerminalShareDefault::ReadRun => ShareClick::Share(TerminalAccess::ReadRun),
    }
}

/// The setting a dialog choice saves when "Remember this" is ticked.
pub fn remembered_default(access: TerminalAccess) -> TerminalShareDefault {
    match access {
        TerminalAccess::None => TerminalShareDefault::Ask,
        TerminalAccess::Read => TerminalShareDefault::ReadOnly,
        TerminalAccess::ReadRun => TerminalShareDefault::ReadRun,
    }
}

/// The eye icon's tooltip: the tab's current access, and what a click does.
pub fn share_tooltip(access: TerminalAccess) -> &'static str {
    match access {
        TerminalAccess::None => "Not shared with the agent. Click to share",
        TerminalAccess::Read => "Shared with the agent: read only. Click to stop sharing",
        TerminalAccess::ReadRun => "Shared with the agent: read + run. Click to stop sharing",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chatty_terminal::TerminalConfig;
    use std::time::{Duration, Instant};

    fn bash() -> Arc<TerminalHandle> {
        let config = TerminalConfig {
            shell: Some("bash".into()),
            args: vec!["--norc".into(), "--noprofile".into(), "-i".into()],
            ..TerminalConfig::default()
        };
        let (handle, _events) = TerminalHandle::spawn(config).expect("bash starts");
        Arc::new(handle)
    }

    fn register(reg: &EmbeddedTerminals, handle: &Arc<TerminalHandle>) -> String {
        reg.register(
            handle,
            TerminalKind::Human,
            "bash".into(),
            std::env::temp_dir(),
        )
    }

    async fn read_until(reg: &EmbeddedTerminals, id: &str, want: &str) -> String {
        let start = Instant::now();
        loop {
            let last = match reg.read(id, Region::Screen).await {
                Ok(text) if text.text.contains(want) => return text.text,
                Ok(text) => text.text,
                Err(e) => e.to_string(),
            };
            assert!(
                start.elapsed() < Duration::from_secs(30),
                "never saw {want:?}; last: {last}"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// Human tabs start unshared: listed with `access: none`, never read.
    /// Shared, they read; unshared again, they are refused again.
    #[tokio::test]
    async fn a_human_tab_is_read_only_while_shared() {
        let reg = EmbeddedTerminals::default();
        let handle = bash();
        let id = register(&reg, &handle);

        let list = reg.list().await;
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, id);
        assert_eq!(list[0].backend, TerminalBackend::Embedded);
        assert_eq!(list[0].kind, TerminalKind::Human);
        assert_eq!(list[0].access, TerminalAccess::None);
        let err = reg.read(&id, Region::Screen).await.unwrap_err();
        assert!(err.to_string().contains("not shared"), "{err}");

        reg.set_access(&id, TerminalAccess::Read);
        assert_eq!(reg.list().await[0].access, TerminalAccess::Read);
        handle.write(b"echo shared-$((6*7))\r").unwrap();
        read_until(&reg, &id, "shared-42").await;

        reg.set_access(&id, TerminalAccess::None);
        assert!(reg.read(&id, Region::Screen).await.is_err());
    }

    /// The list is most recently focused first; a closed tab drops out.
    #[tokio::test]
    async fn list_follows_focus_and_forgets_closed_tabs() {
        let reg = EmbeddedTerminals::default();
        let (a, b) = (bash(), bash());
        let (id_a, id_b) = (register(&reg, &a), register(&reg, &b));
        let ids = |list: Vec<TerminalInfo>| list.into_iter().map(|t| t.id).collect::<Vec<_>>();
        assert_eq!(ids(reg.list().await), [id_b.clone(), id_a.clone()]);
        reg.focused(&id_a);
        assert_eq!(ids(reg.list().await), [id_a.clone(), id_b.clone()]);

        drop(a);
        assert_eq!(ids(reg.list().await), std::slice::from_ref(&id_b));
        reg.remove(&id_b);
        assert!(reg.list().await.is_empty());
    }

    /// At a password prompt (echo off) a shared tab's screen is never
    /// returned, and a read is announced to the dock only when it happened.
    #[tokio::test]
    async fn a_hidden_input_prompt_is_reported_instead_of_the_screen() {
        let reg = EmbeddedTerminals::default();
        let handle = bash();
        let id = register(&reg, &handle);
        reg.set_access(&id, TerminalAccess::Read);
        let mut reads = reg.subscribe_reads();
        read_until(&reg, &id, "$").await;
        assert_eq!(reads.try_recv().unwrap(), id);

        handle.write(b"read -s -p 'Password: ' secret\r").unwrap();
        let start = Instant::now();
        let err = loop {
            match reg.read(&id, Region::Screen).await {
                Err(e) => break e.to_string(),
                Ok(_) => {
                    assert!(start.elapsed() < Duration::from_secs(30), "never hidden");
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
        };
        assert_eq!(err, HIDDEN_INPUT_MESSAGE);
        while reads.try_recv().is_ok() {}
        assert!(reg.read(&id, Region::Screen).await.is_err());
        assert!(reads.try_recv().is_err(), "a refused read is not announced");

        // Answered (here: interrupted), the screen reads again.
        handle.write(b"\x03").unwrap();
        read_until(&reg, &id, "Password:").await;
    }

    /// The agent's own tab is readable from the start and not the human's
    /// to unshare.
    #[tokio::test]
    async fn the_agent_tab_is_always_readable() {
        let reg = EmbeddedTerminals::default();
        let handle = bash();
        let id = reg.register(&handle, TerminalKind::Agent, "bash".into(), PathBuf::new());
        assert_eq!(reg.access(&id), TerminalAccess::Read);
        reg.set_access(&id, TerminalAccess::None);
        assert_eq!(reg.access(&id), TerminalAccess::Read);
        assert_eq!(reg.list().await[0].kind, TerminalKind::Agent);
    }

    /// The eye icon: unshared + Ask opens the dialog; a remembered choice
    /// shares at once; a shared tab unshares at once.
    #[test]
    fn remembered_choice_skips_the_dialog() {
        use TerminalShareDefault as D;
        assert_eq!(share_click(TerminalAccess::None, D::Ask), ShareClick::Ask);
        assert_eq!(
            share_click(TerminalAccess::None, D::ReadOnly),
            ShareClick::Share(TerminalAccess::Read)
        );
        assert_eq!(
            share_click(TerminalAccess::None, D::ReadRun),
            ShareClick::Share(TerminalAccess::ReadRun)
        );
        for remembered in [D::Ask, D::ReadOnly, D::ReadRun] {
            assert_eq!(
                share_click(TerminalAccess::Read, remembered),
                ShareClick::Unshare
            );
            assert_eq!(
                share_click(TerminalAccess::ReadRun, remembered),
                ShareClick::Unshare
            );
        }
        assert_eq!(remembered_default(TerminalAccess::Read), D::ReadOnly);
        assert_eq!(remembered_default(TerminalAccess::ReadRun), D::ReadRun);
    }
}
