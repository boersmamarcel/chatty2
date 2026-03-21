//! Wry/tao browser backend — real WebView integration.
//!
//! # Architecture (Linux / Windows)
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
//! for the result.
//!
//! # macOS
//!
//! On macOS, tao requires `EventLoop` on the main thread. Since GPUI already
//! occupies the main thread, `WryBackend::new()` returns an error and the
//! caller falls back to HTTP mode. A future phase will integrate WebViews
//! via GPUI's main-thread dispatch.

use async_trait::async_trait;

use super::{BrowserBackend, Cookie, TabId, TabInfo};

// ══════════════════════════════════════════════════════════════════════════════
// Platform-specific inner module
// ══════════════════════════════════════════════════════════════════════════════

// On Linux/Windows we have the full tao + wry event loop implementation.
// On macOS the inner module is empty — WryBackend::new() always bails before
// any tao/wry types are touched.

#[cfg(not(target_os = "macos"))]
mod inner {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    use tokio::sync::oneshot;

    use crate::backend::{TabId, TabInfo};

    // ── Type aliases ────────────────────────────────────────────────────

    /// Pending JS callback senders, keyed by tab ID.
    pub(super) type JsCallbackMap = HashMap<String, Vec<oneshot::Sender<anyhow::Result<String>>>>;

    /// Pending load-wait senders with deadlines, keyed by tab ID.
    pub(super) type LoadWaiterMap =
        HashMap<String, Vec<(oneshot::Sender<anyhow::Result<()>>, std::time::Instant)>>;

    // ── Command types ───────────────────────────────────────────────────

    /// Commands sent from async callers to the event loop thread.
    #[allow(dead_code)] // Variants constructed via send() which the compiler can't trace
    pub(super) enum WryCommand {
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
        JsResult { tab_id: String, result: String },
    }

    // Safety: WryCommand is sent between threads via EventLoopProxy.
    // All fields are Send (oneshot::Sender, String, TabId).
    unsafe impl Send for WryCommand {}

    // ── Tab state ───────────────────────────────────────────────────────

    /// Per-tab state owned by the event loop thread.
    pub(super) struct TabState {
        #[allow(dead_code)]
        pub(super) window: tao::window::Window,
        pub(super) webview: wry::WebView,
        pub(super) url: String,
        pub(super) title: String,
    }

    // ── Event loop runner ───────────────────────────────────────────────

    /// Run the tao event loop on the current thread (never returns).
    pub(super) fn run_event_loop(
        proxy_tx: std::sync::mpsc::Sender<tao::event_loop::EventLoopProxy<WryCommand>>,
        tabs_snapshot: Arc<Mutex<Vec<TabInfo>>>,
    ) {
        use tao::event::{Event, WindowEvent};
        use tao::event_loop::{ControlFlow, EventLoopBuilder};

        let event_loop = EventLoopBuilder::<WryCommand>::with_user_event().build();
        let proxy = event_loop.create_proxy();
        let ipc_proxy = event_loop.create_proxy();

        if proxy_tx.send(proxy).is_err() {
            tracing::error!("Failed to send EventLoopProxy — caller dropped");
            return;
        }

        let mut tabs: HashMap<String, TabState> = HashMap::new();
        let mut next_tab_id: u64 = 1;
        let mut js_callbacks: JsCallbackMap = HashMap::new();
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
                    // Tabs are managed via close_tab — ignore OS window close.
                }
                Event::MainEventsCleared => {
                    // Expire timed-out load waiters
                    let now = std::time::Instant::now();
                    for waiters in load_waiters.values_mut() {
                        let mut i = 0;
                        while i < waiters.len() {
                            if waiters[i].0.is_closed() || now >= waiters[i].1 {
                                let (sender, deadline) = waiters.swap_remove(i);
                                if now >= deadline {
                                    let _ = sender.send(Ok(()));
                                }
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

    // ── Command handler ─────────────────────────────────────────────────

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
                        .with_visible(false) // Hidden — requires display server
                        .build(event_loop)
                        .map_err(|e| anyhow::anyhow!("Failed to create window: {e}"))?;

                    let tab_id_for_ipc = tab_id_str.clone();
                    let proxy = ipc_proxy.clone();

                    let webview = wry::WebViewBuilder::new()
                        .with_url("about:blank")
                        .with_ipc_handler(move |msg| {
                            let body = msg.body();
                            if let Some(result) = body.strip_prefix("__chatty_js_result:") {
                                let _ = proxy.send_event(WryCommand::JsResult {
                                    tab_id: tab_id_for_ipc.clone(),
                                    result: result.to_string(),
                                });
                            }
                        })
                        .build(&window)
                        .map_err(|e| anyhow::anyhow!("Failed to create WebView: {e}"))?;

                    tabs.insert(
                        tab_id_str.clone(),
                        TabState {
                            window,
                            webview,
                            url: "about:blank".to_string(),
                            title: String::new(),
                        },
                    );
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
                    let wrapped = format!(
                        r#"(function() {{
                            try {{
                                var __r = (function() {{ {script} }})();
                                if (__r === undefined) __r = null;
                                window.ipc.postMessage("__chatty_js_result:" + JSON.stringify(__r));
                            }} catch(e) {{
                                window.ipc.postMessage("__chatty_js_result:" + JSON.stringify("__error:" + e.toString()));
                            }}
                        }})()"#
                    );
                    match tab.webview.evaluate_script(&wrapped) {
                        Ok(()) => {
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

            WryCommand::JsResult { tab_id, result } => {
                if let Some(waiters) = js_callbacks.get_mut(&tab_id) {
                    if let Some(reply) = waiters.pop() {
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
                // Resolve pending load waiters — JS executing means the page loaded
                if let Some(waiters) = load_waiters.remove(&tab_id) {
                    for (reply, _) in waiters {
                        let _ = reply.send(Ok(()));
                    }
                }
            }
        }
    }

    /// Sync the shared tabs snapshot.
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
}

// ══════════════════════════════════════════════════════════════════════════════
// Public WryBackend type
// ══════════════════════════════════════════════════════════════════════════════

/// Wry/tao-based browser backend.
///
/// Manages OS-native WebViews via wry, with tao providing the windowing
/// layer. The event loop runs on a dedicated thread; all async methods
/// communicate with it via `EventLoopProxy` + oneshot channels.
///
/// On macOS, `new()` returns an error because tao requires the main thread
/// (which is occupied by GPUI). The caller should fall back to HTTP mode.
pub struct WryBackend {
    /// Proxy to send commands to the event loop thread.
    /// `None` on macOS (construction always fails before this is set).
    #[cfg(not(target_os = "macos"))]
    proxy: tao::event_loop::EventLoopProxy<inner::WryCommand>,
    #[cfg(not(target_os = "macos"))]
    tabs_snapshot: std::sync::Arc<std::sync::Mutex<Vec<TabInfo>>>,
    #[cfg(not(target_os = "macos"))]
    _thread: Option<std::thread::JoinHandle<()>>,

    // macOS: zero-size struct — new() always bails.
    #[cfg(target_os = "macos")]
    _private: (),
}

/// Convenience impl for tests. Panics if the display server is unavailable
/// or on macOS. Production code should use `WryBackend::new()` and handle
/// the error.
impl Default for WryBackend {
    fn default() -> Self {
        Self::new().expect("WryBackend::new() failed — display server required")
    }
}

impl WryBackend {
    /// Create a new wry backend.
    ///
    /// # Platform notes
    ///
    /// - **macOS**: Returns an error. tao requires `EventLoop` on the main
    ///   thread, which is occupied by GPUI. Browser tools use HTTP fallback.
    /// - **Linux**: Spawns a background thread with tao + wry. Requires
    ///   WebKitGTK (`libwebkit2gtk-4.1-dev`).
    /// - **Windows**: Spawns a background thread with tao + wry. Requires
    ///   WebView2 (bundled with Windows 10+).
    pub fn new() -> anyhow::Result<Self> {
        #[cfg(target_os = "macos")]
        {
            // On macOS, tao's EventLoop must be created on the main thread.
            // GPUI already owns the main thread, so we can't use tao here.
            // Attempting to create tao's EventLoop on a background thread
            // panics and corrupts tao's global state, crashing the process.
            anyhow::bail!(
                "WryBackend is not yet supported on macOS. \
                 The tao event loop must run on the main thread, which is \
                 occupied by GPUI. Browser tools will use HTTP fallback."
            );
        }

        #[cfg(not(target_os = "macos"))]
        {
            use std::sync::{Arc, Mutex};

            let tabs_snapshot: Arc<Mutex<Vec<TabInfo>>> = Arc::new(Mutex::new(Vec::new()));
            let tabs_snapshot_clone = tabs_snapshot.clone();

            let (proxy_tx, proxy_rx) =
                std::sync::mpsc::channel::<tao::event_loop::EventLoopProxy<inner::WryCommand>>();

            let thread = std::thread::Builder::new()
                .name("wry-event-loop".into())
                .spawn(move || {
                    // Catch panics so they don't corrupt global state or
                    // propagate to the main thread.
                    if let Err(e) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        inner::run_event_loop(proxy_tx.clone(), tabs_snapshot_clone);
                    })) {
                        let msg = if let Some(s) = e.downcast_ref::<&str>() {
                            s.to_string()
                        } else if let Some(s) = e.downcast_ref::<String>() {
                            s.clone()
                        } else {
                            "unknown panic".to_string()
                        };
                        tracing::error!(error = %msg, "wry event loop panicked");
                    }
                })
                .map_err(|e| anyhow::anyhow!("Failed to spawn wry event loop thread: {e}"))?;

            let proxy = proxy_rx.recv().map_err(|_| {
                anyhow::anyhow!(
                    "Event loop thread died before sending proxy. \
                     Check that system WebView libraries are installed."
                )
            })?;

            tracing::info!("WryBackend created — event loop thread running");

            Ok(Self {
                proxy,
                tabs_snapshot,
                _thread: Some(thread),
            })
        }
    }

    /// Send a command to the event loop and await the reply.
    #[cfg(not(target_os = "macos"))]
    async fn send<T: Send + 'static>(
        &self,
        make_cmd: impl FnOnce(tokio::sync::oneshot::Sender<T>) -> inner::WryCommand,
    ) -> anyhow::Result<T> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.proxy
            .send_event(make_cmd(tx))
            .map_err(|_| anyhow::anyhow!("Event loop is shut down"))?;
        rx.await
            .map_err(|_| anyhow::anyhow!("Event loop dropped the reply channel"))
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// BrowserBackend trait implementation
// ══════════════════════════════════════════════════════════════════════════════

// On macOS, all methods are unreachable (new() always fails), but they must
// exist to satisfy the trait.

#[async_trait]
impl BrowserBackend for WryBackend {
    async fn new_tab(&self) -> anyhow::Result<TabId> {
        #[cfg(target_os = "macos")]
        anyhow::bail!("WryBackend is not available on macOS");
        #[cfg(not(target_os = "macos"))]
        self.send(|reply| inner::WryCommand::NewTab { reply })
            .await?
    }

    async fn close_tab(&self, tab: &TabId) -> anyhow::Result<()> {
        #[cfg(target_os = "macos")]
        {
            let _ = tab;
            anyhow::bail!("WryBackend is not available on macOS");
        }
        #[cfg(not(target_os = "macos"))]
        self.send(|reply| inner::WryCommand::CloseTab {
            tab_id: tab.clone(),
            reply,
        })
        .await?
    }

    async fn navigate(&self, tab: &TabId, url: &str) -> anyhow::Result<()> {
        #[cfg(target_os = "macos")]
        {
            let _ = (tab, url);
            anyhow::bail!("WryBackend is not available on macOS");
        }
        #[cfg(not(target_os = "macos"))]
        self.send(|reply| inner::WryCommand::Navigate {
            tab_id: tab.clone(),
            url: url.to_string(),
            reply,
        })
        .await?
    }

    async fn current_url(&self, tab: &TabId) -> anyhow::Result<String> {
        #[cfg(target_os = "macos")]
        {
            let _ = tab;
            anyhow::bail!("WryBackend is not available on macOS");
        }
        #[cfg(not(target_os = "macos"))]
        self.send(|reply| inner::WryCommand::CurrentUrl {
            tab_id: tab.clone(),
            reply,
        })
        .await?
    }

    async fn evaluate_js(&self, tab: &TabId, script: &str) -> anyhow::Result<String> {
        #[cfg(target_os = "macos")]
        {
            let _ = (tab, script);
            anyhow::bail!("WryBackend is not available on macOS");
        }
        #[cfg(not(target_os = "macos"))]
        self.send(|reply| inner::WryCommand::EvaluateJs {
            tab_id: tab.clone(),
            script: script.to_string(),
            reply,
        })
        .await?
    }

    async fn get_cookies(&self, tab: &TabId) -> anyhow::Result<Vec<Cookie>> {
        #[cfg(target_os = "macos")]
        {
            let _ = tab;
            anyhow::bail!("WryBackend is not available on macOS");
        }
        #[cfg(not(target_os = "macos"))]
        {
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
            let cleaned = result.trim().trim_matches('"');
            let unescaped = cleaned.replace("\\\"", "\"").replace("\\\\", "\\");
            let cookies: Vec<Cookie> = serde_json::from_str(&unescaped).unwrap_or_default();
            Ok(cookies)
        }
    }

    async fn set_cookies(&self, tab: &TabId, cookies: &[Cookie]) -> anyhow::Result<()> {
        #[cfg(target_os = "macos")]
        {
            let _ = (tab, cookies);
            anyhow::bail!("WryBackend is not available on macOS");
        }
        #[cfg(not(target_os = "macos"))]
        {
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
    }

    async fn screenshot(&self, _tab: &TabId) -> anyhow::Result<Vec<u8>> {
        anyhow::bail!("Screenshot is not supported by the wry backend")
    }

    async fn wait_for_load(&self, tab: &TabId, timeout_ms: u64) -> anyhow::Result<()> {
        #[cfg(target_os = "macos")]
        {
            let _ = (tab, timeout_ms);
            anyhow::bail!("WryBackend is not available on macOS");
        }
        #[cfg(not(target_os = "macos"))]
        self.send(|reply| inner::WryCommand::WaitForLoad {
            tab_id: tab.clone(),
            timeout_ms,
            reply,
        })
        .await?
    }

    fn list_tabs(&self) -> Vec<TabInfo> {
        #[cfg(target_os = "macos")]
        {
            Vec::new()
        }
        #[cfg(not(target_os = "macos"))]
        {
            self.tabs_snapshot
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone()
        }
    }

    async fn shutdown(&self) -> anyhow::Result<()> {
        #[cfg(target_os = "macos")]
        {
            Ok(())
        }
        #[cfg(not(target_os = "macos"))]
        self.send(|reply| inner::WryCommand::Shutdown { reply })
            .await?
    }
}
