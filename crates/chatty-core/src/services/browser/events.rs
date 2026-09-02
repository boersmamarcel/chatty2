//! Console and network capture.
//!
//! Token discipline, decided up front rather than retrofitted: these buffers
//! accumulate in the background and the tools drain them to a *file*, returning
//! a short summary and a path. Dumping every console line into the context after
//! every action is how a browser loop becomes unaffordable.
//!
//! Both buffers are bounded ring buffers. A page in a redirect loop must not be
//! able to grow our memory without limit.

use std::collections::VecDeque;
use std::sync::Arc;

use chromiumoxide::cdp::browser_protocol::log::EventEntryAdded;
use chromiumoxide::cdp::browser_protocol::network::{EventLoadingFailed, EventResponseReceived};
use chromiumoxide::cdp::js_protocol::runtime::{EventConsoleApiCalled, EventExceptionThrown};
use chromiumoxide::page::Page;
use futures::StreamExt;
use parking_lot::Mutex;
use tokio::task::JoinHandle;

use super::error::BrowserError;

/// How many entries each buffer keeps before dropping the oldest.
const MAX_ENTRIES: usize = 1000;

/// One captured console message or page error.
#[derive(Clone, Debug, serde::Serialize)]
pub struct ConsoleEntry {
    /// `error`, `warning`, `info`, `log`, `debug`, or `exception`.
    pub level: String,
    pub text: String,
}

/// One captured network response or failure.
#[derive(Clone, Debug, serde::Serialize)]
pub struct NetworkEntry {
    pub url: String,
    /// HTTP status, or `None` when the request failed before a response.
    pub status: Option<i64>,
    /// Set when the request failed outright.
    pub error: Option<String>,
}

impl ConsoleEntry {
    /// Whether this entry is one the agent almost certainly cares about.
    pub fn is_problem(&self) -> bool {
        matches!(self.level.as_str(), "error" | "exception" | "warning")
    }
}

impl NetworkEntry {
    /// Whether this entry is one the agent almost certainly cares about.
    pub fn is_problem(&self) -> bool {
        self.error.is_some() || self.status.is_some_and(|s| s >= 400)
    }
}

/// Bounded buffers, drained by the `browser_console` / `browser_network` tools.
#[derive(Default)]
pub struct EventBuffers {
    console: Mutex<VecDeque<ConsoleEntry>>,
    network: Mutex<VecDeque<NetworkEntry>>,
    /// Entries dropped because a buffer was full, so the summary can say so.
    console_dropped: Mutex<usize>,
    network_dropped: Mutex<usize>,
}

impl EventBuffers {
    pub fn push_console(&self, entry: ConsoleEntry) {
        let mut buf = self.console.lock();
        if buf.len() == MAX_ENTRIES {
            buf.pop_front();
            *self.console_dropped.lock() += 1;
        }
        buf.push_back(entry);
    }

    pub fn push_network(&self, entry: NetworkEntry) {
        let mut buf = self.network.lock();
        if buf.len() == MAX_ENTRIES {
            buf.pop_front();
            *self.network_dropped.lock() += 1;
        }
        buf.push_back(entry);
    }

    /// Take everything captured since the last drain.
    pub fn drain_console(&self) -> (Vec<ConsoleEntry>, usize) {
        let entries = self.console.lock().drain(..).collect();
        let dropped = std::mem::take(&mut *self.console_dropped.lock());
        (entries, dropped)
    }

    /// Take everything captured since the last drain.
    pub fn drain_network(&self) -> (Vec<NetworkEntry>, usize) {
        let entries = self.network.lock().drain(..).collect();
        let dropped = std::mem::take(&mut *self.network_dropped.lock());
        (entries, dropped)
    }

    /// Drop everything. Called on navigation — entries from the previous page
    /// would otherwise be attributed to the new one.
    pub fn clear(&self) {
        self.console.lock().clear();
        self.network.lock().clear();
        *self.console_dropped.lock() = 0;
        *self.network_dropped.lock() = 0;
    }
}

/// Subscribe to the CDP events we buffer, returning the pump tasks.
pub(super) async fn spawn_listeners(
    page: &Page,
    buffers: Arc<EventBuffers>,
) -> Result<Vec<JoinHandle<()>>, BrowserError> {
    // The domains must be enabled before their events are emitted.
    page.enable_log()
        .await
        .map_err(|e| BrowserError::Protocol(format!("cannot enable Log domain: {e}")))?;
    page.enable_runtime()
        .await
        .map_err(|e| BrowserError::Protocol(format!("cannot enable Runtime domain: {e}")))?;
    // No convenience wrapper for Network on Page — go through the raw command.
    page.execute(chromiumoxide::cdp::browser_protocol::network::EnableParams::default())
        .await
        .map_err(|e| BrowserError::Protocol(format!("cannot enable Network domain: {e}")))?;

    let mut handles = Vec::new();

    let mut console = page
        .event_listener::<EventConsoleApiCalled>()
        .await
        .map_err(|e| BrowserError::Protocol(format!("cannot listen for console events: {e}")))?;
    handles.push(tokio::spawn({
        let buffers = buffers.clone();
        async move {
            while let Some(event) = console.next().await {
                buffers.push_console(ConsoleEntry {
                    level: format!("{:?}", event.r#type).to_lowercase(),
                    text: event
                        .args
                        .iter()
                        .map(describe_remote_object)
                        .collect::<Vec<_>>()
                        .join(" "),
                });
            }
        }
    }));

    let mut exceptions = page
        .event_listener::<EventExceptionThrown>()
        .await
        .map_err(|e| BrowserError::Protocol(format!("cannot listen for exceptions: {e}")))?;
    handles.push(tokio::spawn({
        let buffers = buffers.clone();
        async move {
            while let Some(event) = exceptions.next().await {
                let details = &event.exception_details;
                let text = details
                    .exception
                    .as_ref()
                    .map(describe_remote_object)
                    .unwrap_or_else(|| details.text.clone());
                buffers.push_console(ConsoleEntry {
                    level: "exception".to_string(),
                    text,
                });
            }
        }
    }));

    let mut log = page
        .event_listener::<EventEntryAdded>()
        .await
        .map_err(|e| BrowserError::Protocol(format!("cannot listen for log entries: {e}")))?;
    handles.push(tokio::spawn({
        let buffers = buffers.clone();
        async move {
            while let Some(event) = log.next().await {
                buffers.push_console(ConsoleEntry {
                    level: format!("{:?}", event.entry.level).to_lowercase(),
                    text: event.entry.text.clone(),
                });
            }
        }
    }));

    let mut responses = page
        .event_listener::<EventResponseReceived>()
        .await
        .map_err(|e| BrowserError::Protocol(format!("cannot listen for responses: {e}")))?;
    handles.push(tokio::spawn({
        let buffers = buffers.clone();
        async move {
            while let Some(event) = responses.next().await {
                buffers.push_network(NetworkEntry {
                    url: event.response.url.clone(),
                    status: Some(event.response.status),
                    error: None,
                });
            }
        }
    }));

    let mut failures = page
        .event_listener::<EventLoadingFailed>()
        .await
        .map_err(|e| BrowserError::Protocol(format!("cannot listen for failed requests: {e}")))?;
    handles.push(tokio::spawn({
        let buffers = buffers.clone();
        async move {
            while let Some(event) = failures.next().await {
                buffers.push_network(NetworkEntry {
                    // CDP reports the failure against a request id; the URL is
                    // on the matching requestWillBeSent, which we do not buffer.
                    // The error text plus the type is what the agent acts on.
                    url: format!("({:?} request)", event.r#type),
                    status: None,
                    error: Some(event.error_text.clone()),
                });
            }
        }
    }));

    Ok(handles)
}

/// Best-effort rendering of a console argument.
fn describe_remote_object(
    object: &chromiumoxide::cdp::js_protocol::runtime::RemoteObject,
) -> String {
    if let Some(value) = &object.value {
        match value {
            serde_json::Value::String(s) => s.clone(),
            other => other.to_string(),
        }
    } else if let Some(description) = &object.description {
        description.clone()
    } else {
        format!("{:?}", object.r#type)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn console(level: &str) -> ConsoleEntry {
        ConsoleEntry {
            level: level.to_string(),
            text: "x".to_string(),
        }
    }

    #[test]
    fn drain_takes_everything_and_leaves_the_buffer_empty() {
        let buffers = EventBuffers::default();
        buffers.push_console(console("log"));
        buffers.push_console(console("error"));

        let (entries, dropped) = buffers.drain_console();
        assert_eq!(entries.len(), 2);
        assert_eq!(dropped, 0);

        let (entries, _) = buffers.drain_console();
        assert!(
            entries.is_empty(),
            "a second drain returns only new entries"
        );
    }

    #[test]
    fn console_buffer_is_bounded_and_reports_drops() {
        let buffers = EventBuffers::default();
        for _ in 0..(MAX_ENTRIES + 5) {
            buffers.push_console(console("log"));
        }
        let (entries, dropped) = buffers.drain_console();
        assert_eq!(entries.len(), MAX_ENTRIES);
        assert_eq!(dropped, 5);
    }

    #[test]
    fn network_buffer_is_bounded_and_reports_drops() {
        let buffers = EventBuffers::default();
        for _ in 0..(MAX_ENTRIES + 3) {
            buffers.push_network(NetworkEntry {
                url: "http://localhost/".into(),
                status: Some(200),
                error: None,
            });
        }
        let (entries, dropped) = buffers.drain_network();
        assert_eq!(entries.len(), MAX_ENTRIES);
        assert_eq!(dropped, 3);
    }

    #[test]
    fn clear_drops_both_buffers_and_the_drop_counts() {
        let buffers = EventBuffers::default();
        buffers.push_console(console("error"));
        buffers.push_network(NetworkEntry {
            url: "http://localhost/".into(),
            status: Some(500),
            error: None,
        });
        buffers.clear();

        let (console_entries, console_dropped) = buffers.drain_console();
        let (network_entries, network_dropped) = buffers.drain_network();
        assert!(console_entries.is_empty());
        assert!(network_entries.is_empty());
        assert_eq!(console_dropped, 0);
        assert_eq!(network_dropped, 0);
    }

    #[test]
    fn problem_classification() {
        assert!(console("error").is_problem());
        assert!(console("exception").is_problem());
        assert!(console("warning").is_problem());
        assert!(!console("log").is_problem());
        assert!(!console("info").is_problem());

        let ok = NetworkEntry {
            url: "u".into(),
            status: Some(200),
            error: None,
        };
        let missing = NetworkEntry {
            url: "u".into(),
            status: Some(404),
            error: None,
        };
        let failed = NetworkEntry {
            url: "u".into(),
            status: None,
            error: Some("net::ERR_CONNECTION_REFUSED".into()),
        };
        assert!(!ok.is_problem());
        assert!(missing.is_problem());
        assert!(failed.is_problem());
    }
}
