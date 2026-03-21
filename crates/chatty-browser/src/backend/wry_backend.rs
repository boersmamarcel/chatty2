//! Wry/tao browser backend — real WebView integration.
//!
//! Architecture:
//!
//! ```text
//! ┌─────────────┐        EventLoopProxy         ┌─────────────────┐
//! │ Tokio tasks  │ ──── (WryCommand + oneshot) ──►│ Tao event loop  │
//! │ (async trait │ ◄─── oneshot::Sender result ──│ thread (owns     │
//! │  methods)    │                                │  WebViews)       │
//! └─────────────┘                                └─────────────────┘
//! ```
//!
//! The tao event loop runs on a **dedicated OS thread** (required because
//! wry's `WebView` is `!Send`). All `BrowserBackend` async methods send a
//! `WryCommand` via the `EventLoopProxy`, each carrying a `oneshot::Sender`
//! for the result. The event loop processes commands, interacts with the
//! `WebView`, and sends results back.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use tokio::sync::oneshot;

use super::{BrowserBackend, Cookie, TabId, TabInfo};

// ── Type aliases ────────────────────────────────────────────────────────────

/// Pending JS callback senders, keyed by tab ID.
type JsCallbackMap = HashMap<String, Vec<oneshot::Sender<anyhow::Result<String>>>>;

/// Pending load-wait senders with deadlines, keyed by tab ID.
type LoadWaiterMap =
    HashMap<String, Vec<(oneshot::Sender<anyhow::Result<()>>, std::time::Instant)>>;

// ── Command types ───────────────────────────────────────────────────────────

/// Commands sent from async callers to the event loop thread.
#[allow(dead_code)] // Variants constructed via send() which the compiler can't trace
enum WryCommand {
    NewTab {
        reply: oneshot::Sender<anyhow::Result<TabId>>,
    },
    CloseTab {
        tab_id: TabId,
        reply: oneshot::Sender<anyhow::Result<()>>,
    },
    Navigate {
        tab_id: TabId,
        url: String,
        reply: oneshot::Sender<anyhow::Result<()>>,
    },
    CurrentUrl {
        tab_id: TabId,
        reply: oneshot::Sender<anyhow::Result<String>>,
    },
    EvaluateJs {
        tab_id: TabId,
        script: String,
        reply: oneshot::Sender<anyhow::Result<String>>,
    },
    WaitForLoad {
        tab_id: TabId,
        timeout_ms: u64,
        reply: oneshot::Sender<anyhow::Result<()>>,
    },
    ListTabs {
        reply: oneshot::Sender<Vec<TabInfo>>,
    },
    Shutdown {
        reply: oneshot::Sender<anyhow::Result<()>>,
    },
    /// Internal: delivers a JS evaluation result from the IPC handler.
    _JsResult { tab_id: String, result: String },
}

// Safety: WryCommand is sent between threads via EventLoopProxy.
// All fields are Send (oneshot::Sender, String, TabId).
// The _JsResult variant only contains Strings.
unsafe impl Send for WryCommand {}

// ── Tab state tracked on the event loop thread ──────────────────────────────

/// Per-tab state owned by the event loop thread.
struct TabState {
    #[allow(dead_code)]
    window: tao::window::Window,
    webview: wry::WebView,
    url: String,
    title: String,
}

// ── WryBackend ──────────────────────────────────────────────────────────────

/// Wry/tao-based browser backend.
///
/// Manages OS-native WebViews via wry, with tao providing the windowing
/// layer. The event loop runs on a dedicated thread; all async methods
/// communicate with it via `EventLoopProxy` + oneshot channels.
pub struct WryBackend {
    proxy: tao::event_loop::EventLoopProxy<WryCommand>,
    /// Tabs snapshot for the sync `list_tabs()` method.
    tabs_snapshot: Arc<Mutex<Vec<TabInfo>>>,
    /// Handle to the event loop thread (joined on shutdown).
    _thread: Option<std::thread::JoinHandle<()>>,
}

impl Default for WryBackend {
    fn default() -> Self {
        Self::new().expect("Failed to create WryBackend")
    }
}

impl WryBackend {
    /// Create a new wry backend, spawning the event loop thread.
    pub fn new() -> anyhow::Result<Self> {
        let tabs_snapshot: Arc<Mutex<Vec<TabInfo>>> = Arc::new(Mutex::new(Vec::new()));
        let tabs_snapshot_clone = tabs_snapshot.clone();

        // Channel to get the EventLoopProxy from the spawned thread.
        let (proxy_tx, proxy_rx) =
            std::sync::mpsc::channel::<tao::event_loop::EventLoopProxy<WryCommand>>();

        let thread = std::thread::Builder::new()
            .name("wry-event-loop".into())
            .spawn(move || {
                run_event_loop(proxy_tx, tabs_snapshot_clone);
            })
            .map_err(|e| anyhow::anyhow!("Failed to spawn wry event loop thread: {e}"))?;

        // Wait for the proxy to be sent from the event loop thread.
        let proxy = proxy_rx
            .recv()
            .map_err(|_| anyhow::anyhow!("Event loop thread died before sending proxy"))?;

        tracing::info!("WryBackend created — event loop thread running");

        Ok(Self {
            proxy,
            tabs_snapshot,
            _thread: Some(thread),
        })
    }

    /// Send a command to the event loop and await the reply.
    async fn send<T: Send + 'static>(
        &self,
        make_cmd: impl FnOnce(oneshot::Sender<T>) -> WryCommand,
    ) -> anyhow::Result<T> {
        let (tx, rx) = oneshot::channel();
        self.proxy
            .send_event(make_cmd(tx))
            .map_err(|_| anyhow::anyhow!("Event loop is shut down"))?;
        rx.await
            .map_err(|_| anyhow::anyhow!("Event loop dropped the reply channel"))
    }
}

#[async_trait]
impl BrowserBackend for WryBackend {
    async fn new_tab(&self) -> anyhow::Result<TabId> {
        self.send(|reply| WryCommand::NewTab { reply }).await?
    }

    async fn close_tab(&self, tab: &TabId) -> anyhow::Result<()> {
        self.send(|reply| WryCommand::CloseTab {
            tab_id: tab.clone(),
            reply,
        })
        .await?
    }

    async fn navigate(&self, tab: &TabId, url: &str) -> anyhow::Result<()> {
        self.send(|reply| WryCommand::Navigate {
            tab_id: tab.clone(),
            url: url.to_string(),
            reply,
        })
        .await?
    }

    async fn current_url(&self, tab: &TabId) -> anyhow::Result<String> {
        self.send(|reply| WryCommand::CurrentUrl {
            tab_id: tab.clone(),
            reply,
        })
        .await?
    }

    async fn evaluate_js(&self, tab: &TabId, script: &str) -> anyhow::Result<String> {
        self.send(|reply| WryCommand::EvaluateJs {
            tab_id: tab.clone(),
            script: script.to_string(),
            reply,
        })
        .await?
    }

    async fn get_cookies(&self, tab: &TabId) -> anyhow::Result<Vec<Cookie>> {
        // Cookies are managed via JS (document.cookie) — delegate to evaluate_js.
        let script = r#"
            (function() {
                var cookies = document.cookie.split('; ').filter(Boolean).map(function(c) {
                    var parts = c.split('=');
                    return {
                        name: parts[0],
                        value: parts.slice(1).join('='),
                        domain: window.location.hostname,
                        path: '/',
                        secure: window.location.protocol === 'https:',
                        http_only: false
                    };
                });
                return JSON.stringify(cookies);
            })()
        "#;
        let result = self.evaluate_js(tab, script).await?;
        // evaluate_script_with_callback returns JSON-stringified result,
        // which means the JSON string itself is quoted. Strip outer quotes.
        let cleaned = result.trim().trim_matches('"');
        // Unescape any escaped quotes inside
        let unescaped = cleaned.replace("\\\"", "\"").replace("\\\\", "\\");
        let cookies: Vec<Cookie> = serde_json::from_str(&unescaped).unwrap_or_default();
        Ok(cookies)
    }

    async fn set_cookies(&self, tab: &TabId, cookies: &[Cookie]) -> anyhow::Result<()> {
        for cookie in cookies {
            let js = format!(
                "document.cookie = '{}={}; path={}; domain={}{}'",
                crate::session::escape_js_string(&cookie.name),
                crate::session::escape_js_string(&cookie.value),
                crate::session::escape_js_string(&cookie.path),
                crate::session::escape_js_string(&cookie.domain),
                if cookie.secure { "; secure" } else { "" },
            );
            self.evaluate_js(tab, &js).await?;
        }
        Ok(())
    }

    async fn screenshot(&self, _tab: &TabId) -> anyhow::Result<Vec<u8>> {
        // wry does not expose a screenshot API
        anyhow::bail!("Screenshot is not supported by the wry backend")
    }

    async fn wait_for_load(&self, tab: &TabId, timeout_ms: u64) -> anyhow::Result<()> {
        self.send(|reply| WryCommand::WaitForLoad {
            tab_id: tab.clone(),
            timeout_ms,
            reply,
        })
        .await?
    }

    fn list_tabs(&self) -> Vec<TabInfo> {
        self.tabs_snapshot
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    async fn shutdown(&self) -> anyhow::Result<()> {
        self.send(|reply| WryCommand::Shutdown { reply }).await?
    }
}

// ── Event loop implementation ───────────────────────────────────────────────

/// Run the tao event loop on the current thread.
///
/// This function never returns (on most platforms `event_loop.run()` is `!`).
/// It sends the `EventLoopProxy` back to the caller via `proxy_tx` so the
/// async side can communicate.
fn run_event_loop(
    proxy_tx: std::sync::mpsc::Sender<tao::event_loop::EventLoopProxy<WryCommand>>,
    tabs_snapshot: Arc<Mutex<Vec<TabInfo>>>,
) {
    use tao::event::{Event, WindowEvent};
    use tao::event_loop::{ControlFlow, EventLoopBuilder};

    let event_loop = EventLoopBuilder::<WryCommand>::with_user_event().build();
    let proxy = event_loop.create_proxy();

    // Create a second proxy for creating IPC handlers inside the event loop.
    let ipc_proxy = event_loop.create_proxy();

    // Send the proxy back to the WryBackend constructor.
    if proxy_tx.send(proxy).is_err() {
        tracing::error!("Failed to send EventLoopProxy — caller dropped");
        return;
    }

    let mut tabs: HashMap<String, TabState> = HashMap::new();
    let mut next_tab_id: u64 = 1;

    // Pending JS evaluation callbacks: tab_id → Vec<oneshot::Sender>
    // (multiple evaluations can be in flight)
    let mut js_callbacks: JsCallbackMap = HashMap::new();

    // Pending wait_for_load callbacks: tab_id → Vec<(Sender, deadline)>
    let mut load_waiters: LoadWaiterMap = HashMap::new();

    event_loop.run(move |event, event_loop_window_target, control_flow| {
        *control_flow = ControlFlow::Wait;

        match event {
            Event::UserEvent(cmd) => {
                handle_command(
                    cmd,
                    &mut tabs,
                    &mut next_tab_id,
                    &mut js_callbacks,
                    &mut load_waiters,
                    &tabs_snapshot,
                    event_loop_window_target,
                    &ipc_proxy,
                    control_flow,
                );
            }
            Event::WindowEvent {
                event: WindowEvent::CloseRequested,
                ..
            } => {
                // Tabs are managed explicitly via close_tab — ignore window close.
            }
            Event::MainEventsCleared => {
                // Check for timed-out load waiters
                let now = std::time::Instant::now();
                for waiters in load_waiters.values_mut() {
                    let mut i = 0;
                    while i < waiters.len() {
                        if waiters[i].0.is_closed() || now >= waiters[i].1 {
                            let (sender, _deadline) = waiters.swap_remove(i);
                            if now >= _deadline {
                                let _ = sender.send(Ok(()));
                            }
                            // Don't increment i — swap_remove put a new element at i
                        } else {
                            i += 1;
                        }
                    }
                }
                load_waiters.retain(|_, v| !v.is_empty());
            }
            _ => {}
        }
    });
}

/// Handle a single command on the event loop thread.
#[allow(clippy::too_many_arguments)]
fn handle_command(
    cmd: WryCommand,
    tabs: &mut HashMap<String, TabState>,
    next_tab_id: &mut u64,
    js_callbacks: &mut JsCallbackMap,
    load_waiters: &mut LoadWaiterMap,
    tabs_snapshot: &Arc<Mutex<Vec<TabInfo>>>,
    event_loop: &tao::event_loop::EventLoopWindowTarget<WryCommand>,
    ipc_proxy: &tao::event_loop::EventLoopProxy<WryCommand>,
    control_flow: &mut tao::event_loop::ControlFlow,
) {
    use tao::window::WindowBuilder;

    match cmd {
        WryCommand::NewTab { reply } => {
            let tab_id_str = format!("tab-{}", *next_tab_id);
            *next_tab_id += 1;

            let result = (|| -> anyhow::Result<TabId> {
                let window = WindowBuilder::new()
                    .with_title("Chatty Browser")
                    .with_visible(false) // headless — no visible window
                    .build(event_loop)
                    .map_err(|e| anyhow::anyhow!("Failed to create window: {e}"))?;

                let tab_id_for_ipc = tab_id_str.clone();
                let proxy = ipc_proxy.clone();

                let webview = wry::WebViewBuilder::new()
                    .with_url("about:blank")
                    .with_ipc_handler(move |msg| {
                        // IPC messages from JS → Rust.
                        // Format: "__chatty_js_result:<json_result>"
                        let body = msg.body();
                        if let Some(result) = body.strip_prefix("__chatty_js_result:") {
                            let _ = proxy.send_event(WryCommand::_JsResult {
                                tab_id: tab_id_for_ipc.clone(),
                                result: result.to_string(),
                            });
                        }
                    })
                    .build(&window)
                    .map_err(|e| anyhow::anyhow!("Failed to create WebView: {e}"))?;

                let tab = TabState {
                    window,
                    webview,
                    url: "about:blank".to_string(),
                    title: String::new(),
                };

                tabs.insert(tab_id_str.clone(), tab);
                update_tabs_snapshot(tabs, tabs_snapshot);

                Ok(TabId(tab_id_str))
            })();

            let _ = reply.send(result);
        }

        WryCommand::CloseTab { tab_id, reply } => {
            let result = if tabs.remove(&tab_id.0).is_some() {
                js_callbacks.remove(&tab_id.0);
                load_waiters.remove(&tab_id.0);
                update_tabs_snapshot(tabs, tabs_snapshot);
                Ok(())
            } else {
                Err(anyhow::anyhow!("Tab {} not found", tab_id.0))
            };
            let _ = reply.send(result);
        }

        WryCommand::Navigate { tab_id, url, reply } => {
            let result = if let Some(tab) = tabs.get_mut(&tab_id.0) {
                tab.url = url.clone();
                tab.webview
                    .load_url(&url)
                    .map_err(|e| anyhow::anyhow!("Navigate failed: {e}"))
            } else {
                Err(anyhow::anyhow!("Tab {} not found", tab_id.0))
            };
            if result.is_ok() {
                update_tabs_snapshot(tabs, tabs_snapshot);
            }
            let _ = reply.send(result);
        }

        WryCommand::CurrentUrl { tab_id, reply } => {
            let result = if let Some(tab) = tabs.get(&tab_id.0) {
                Ok(tab.url.clone())
            } else {
                Err(anyhow::anyhow!("Tab {} not found", tab_id.0))
            };
            let _ = reply.send(result);
        }

        WryCommand::EvaluateJs {
            tab_id,
            script,
            reply,
        } => {
            if let Some(tab) = tabs.get(&tab_id.0) {
                // Wrap the script to capture the return value via IPC
                let wrapped = format!(
                    r#"(function() {{
                        try {{
                            var __chatty_result = (function() {{ {script} }})();
                            if (__chatty_result === undefined) __chatty_result = null;
                            window.ipc.postMessage("__chatty_js_result:" + JSON.stringify(__chatty_result));
                        }} catch(e) {{
                            window.ipc.postMessage("__chatty_js_result:" + JSON.stringify("__error:" + e.toString()));
                        }}
                    }})()"#
                );

                match tab.webview.evaluate_script(&wrapped) {
                    Ok(()) => {
                        // Store the reply sender; it'll be resolved by the IPC handler
                        js_callbacks
                            .entry(tab_id.0.clone())
                            .or_default()
                            .push(reply);
                    }
                    Err(e) => {
                        let _ = reply.send(Err(anyhow::anyhow!("evaluate_script failed: {e}")));
                    }
                }
            } else {
                let _ = reply.send(Err(anyhow::anyhow!("Tab {} not found", tab_id.0)));
            }
        }

        WryCommand::WaitForLoad {
            tab_id,
            timeout_ms,
            reply,
        } => {
            if tabs.contains_key(&tab_id.0) {
                let deadline =
                    std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
                load_waiters
                    .entry(tab_id.0.clone())
                    .or_default()
                    .push((reply, deadline));
            } else {
                let _ = reply.send(Err(anyhow::anyhow!("Tab {} not found", tab_id.0)));
            }
        }

        WryCommand::ListTabs { reply } => {
            let infos: Vec<TabInfo> = tabs
                .iter()
                .map(|(id, state)| TabInfo {
                    id: TabId(id.clone()),
                    url: state.url.clone(),
                    title: state.title.clone(),
                })
                .collect();
            let _ = reply.send(infos);
        }

        WryCommand::Shutdown { reply } => {
            tabs.clear();
            js_callbacks.clear();
            load_waiters.clear();
            update_tabs_snapshot(tabs, tabs_snapshot);
            let _ = reply.send(Ok(()));
            *control_flow = tao::event_loop::ControlFlow::Exit;
        }

        // Internal: JS result delivered via IPC
        WryCommand::_JsResult { tab_id, result } => {
            if let Some(waiters) = js_callbacks.get_mut(&tab_id) {
                if let Some(reply) = waiters.pop() {
                    // Check for error sentinel
                    if let Some(err_msg) = result.strip_prefix("\"__error:") {
                        let err_msg = err_msg.trim_end_matches('"');
                        let _ = reply.send(Err(anyhow::anyhow!("JS error: {err_msg}")));
                    } else {
                        let _ = reply.send(Ok(result));
                    }
                }
                if waiters.is_empty() {
                    js_callbacks.remove(&tab_id);
                }
            }

            // Also resolve any pending load waiters for this tab
            // (JS executing means the page has loaded enough to run scripts)
            if let Some(waiters) = load_waiters.remove(&tab_id) {
                for (reply, _deadline) in waiters {
                    let _ = reply.send(Ok(()));
                }
            }
        }
    }
}

/// Update the shared tabs snapshot (used by the sync `list_tabs` method).
fn update_tabs_snapshot(tabs: &HashMap<String, TabState>, snapshot: &Arc<Mutex<Vec<TabInfo>>>) {
    let infos: Vec<TabInfo> = tabs
        .iter()
        .map(|(id, state)| TabInfo {
            id: TabId(id.clone()),
            url: state.url.clone(),
            title: state.title.clone(),
        })
        .collect();
    if let Ok(mut guard) = snapshot.lock() {
        *guard = infos;
    }
}
