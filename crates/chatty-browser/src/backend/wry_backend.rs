//! Wry browser backend — real WebView integration.
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
//! On macOS, tao's `EventLoop` must run on the main thread — which GPUI
//! already occupies. Instead we bypass tao entirely and use wry's `WebView`
//! with a hidden `NSWindow` created via `objc2`. All WebView operations are
//! dispatched to the main thread via GCD (`dispatch_async`), where they
//! integrate naturally with GPUI's Cocoa run loop. Communication uses the
//! same oneshot-channel pattern as the Linux/Windows backend.

use async_trait::async_trait;

use super::{BrowserBackend, Cookie, TabId, TabInfo};

// ══════════════════════════════════════════════════════════════════════════════
// macOS: native WKWebView via objc2 + wry, dispatched to main thread via GCD
// ══════════════════════════════════════════════════════════════════════════════

#[cfg(target_os = "macos")]
mod inner_macos {
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::ffi::c_void;
    use std::sync::{Arc, Mutex};

    use tokio::sync::oneshot;

    use crate::backend::{TabId, TabInfo};

    // ── GCD dispatch helpers ────────────────────────────────────────────

    #[link(name = "System", kind = "dylib")]
    unsafe extern "C" {
        // `dispatch_get_main_queue()` is a C inline function / macro, not a
        // linkable symbol.  The underlying exported symbol is `_dispatch_main_q`.
        static _dispatch_main_q: c_void;
        fn dispatch_async_f(
            queue: *mut c_void,
            context: *mut c_void,
            work: unsafe extern "C" fn(*mut c_void),
        );
    }

    /// Dispatch a `Send + 'static` closure to the main thread asynchronously.
    fn dispatch_main_async<F: FnOnce() + Send + 'static>(f: F) {
        let boxed: Box<F> = Box::new(f);
        let raw = Box::into_raw(boxed) as *mut c_void;

        unsafe extern "C" fn trampoline<F: FnOnce() + Send + 'static>(ctx: *mut c_void) {
            let f = unsafe { Box::from_raw(ctx as *mut F) };
            f();
        }

        unsafe {
            let main_queue = &raw const _dispatch_main_q as *mut c_void;
            dispatch_async_f(main_queue, raw, trampoline::<F>);
        }
    }

    /// Dispatch a closure to the main thread and await the result.
    pub(super) async fn dispatch_main<T: Send + 'static>(
        f: impl FnOnce() -> T + Send + 'static,
    ) -> T {
        let (tx, rx) = oneshot::channel();
        dispatch_main_async(move || {
            let result = f();
            let _ = tx.send(result);
        });
        rx.await.expect("GCD dispatch callback was dropped")
    }

    // ── Thread-local WebView state (main thread only) ───────────────────

    struct TabState {
        #[allow(dead_code)]
        window: HiddenWindow,
        webview: wry::WebView,
        url: String,
        title: String,
    }

    struct MainThreadState {
        tabs: HashMap<String, TabState>,
        next_tab_id: u64,
        js_callbacks: HashMap<String, Vec<oneshot::Sender<anyhow::Result<String>>>>,
    }

    thread_local! {
        static STATE: RefCell<Option<MainThreadState>> = const { RefCell::new(None) };
    }

    /// Ensure the thread-local state is initialized.
    fn with_state<R>(f: impl FnOnce(&mut MainThreadState) -> R) -> R {
        STATE.with_borrow_mut(|opt| {
            let state = opt.get_or_insert_with(|| MainThreadState {
                tabs: HashMap::new(),
                next_tab_id: 1,
                js_callbacks: HashMap::new(),
            });
            f(state)
        })
    }

    // ── Hidden NSWindow via objc2 ───────────────────────────────────────

    /// A minimal hidden NSWindow for hosting a wry WebView.
    struct HiddenWindow {
        /// Pointer to the NSWindow instance.
        ns_window: *mut c_void,
    }

    // Safety: HiddenWindow is only used on the main thread (enforced by GCD dispatch).
    // The raw pointer is to an NSWindow which is retained by the struct.
    unsafe impl Send for HiddenWindow {}

    impl HiddenWindow {
        /// Create a hidden 1×1 NSWindow on the current thread (must be main).
        fn new() -> anyhow::Result<Self> {
            // Use Objective-C runtime to create a minimal NSWindow.
            // NSWindow *w = [[NSWindow alloc] initWithContentRect:NSMakeRect(0,0,1,1)
            //                styleMask:NSWindowStyleMaskBorderless
            //                backing:NSBackingStoreBuffered
            //                defer:NO];

            #[link(name = "objc", kind = "dylib")]
            unsafe extern "C" {
                fn objc_getClass(name: *const u8) -> *mut c_void;
                fn sel_registerName(name: *const u8) -> *mut c_void;
                fn objc_msgSend() -> *mut c_void; // C varargs ABI – transmuted to match each call site
            }

            // CGRect struct matching NSRect layout
            #[repr(C)]
            #[derive(Copy, Clone)]
            struct CGPoint {
                x: f64,
                y: f64,
            }
            #[repr(C)]
            #[derive(Copy, Clone)]
            struct CGSize {
                width: f64,
                height: f64,
            }
            #[repr(C)]
            #[derive(Copy, Clone)]
            struct CGRect {
                origin: CGPoint,
                size: CGSize,
            }

            unsafe {
                let ns_window_class = objc_getClass(b"NSWindow\0".as_ptr());
                if ns_window_class.is_null() {
                    anyhow::bail!("Failed to get NSWindow class");
                }

                let alloc_sel = sel_registerName(b"alloc\0".as_ptr());
                let init_sel =
                    sel_registerName(b"initWithContentRect:styleMask:backing:defer:\0".as_ptr());
                let set_released_sel = sel_registerName(b"setReleasedWhenClosed:\0".as_ptr());

                // [NSWindow alloc]
                let alloc_fn: unsafe extern "C" fn(*mut c_void, *mut c_void) -> *mut c_void =
                    std::mem::transmute(objc_msgSend as *const ());
                let raw_window = alloc_fn(ns_window_class, alloc_sel);
                if raw_window.is_null() {
                    anyhow::bail!("NSWindow alloc returned nil");
                }

                // initWithContentRect:styleMask:backing:defer:
                let rect = CGRect {
                    origin: CGPoint { x: 0.0, y: 0.0 },
                    size: CGSize {
                        width: 1.0,
                        height: 1.0,
                    },
                };
                let style_mask: u64 = 0; // NSWindowStyleMaskBorderless
                let backing: u64 = 2; // NSBackingStoreBuffered
                let defer: i8 = 0; // NO

                let init_fn: unsafe extern "C" fn(
                    *mut c_void,
                    *mut c_void,
                    CGRect,
                    u64,
                    u64,
                    i8,
                ) -> *mut c_void = std::mem::transmute(objc_msgSend as *const ());
                let window = init_fn(raw_window, init_sel, rect, style_mask, backing, defer);
                if window.is_null() {
                    anyhow::bail!("NSWindow init returned nil");
                }

                // [window setReleasedWhenClosed:NO]
                let set_fn: unsafe extern "C" fn(*mut c_void, *mut c_void, i8) =
                    std::mem::transmute(objc_msgSend as *const ());
                set_fn(window, set_released_sel, 0);

                Ok(HiddenWindow { ns_window: window })
            }
        }
    }

    impl Drop for HiddenWindow {
        fn drop(&mut self) {
            if !self.ns_window.is_null() {
                // [window close] then release
                #[link(name = "objc", kind = "dylib")]
                unsafe extern "C" {
                    fn sel_registerName(name: *const u8) -> *mut c_void;
                    fn objc_msgSend() -> *mut c_void;
                }
                unsafe {
                    let close_sel = sel_registerName(b"close\0".as_ptr());
                    let send: unsafe extern "C" fn(*mut c_void, *mut c_void) =
                        std::mem::transmute(objc_msgSend as *const ());
                    send(self.ns_window, close_sel);

                    let release_sel = sel_registerName(b"release\0".as_ptr());
                    send(self.ns_window, release_sel);
                }
            }
        }
    }

    impl raw_window_handle::HasWindowHandle for HiddenWindow {
        fn window_handle(
            &self,
        ) -> Result<raw_window_handle::WindowHandle<'_>, raw_window_handle::HandleError> {
            let raw = raw_window_handle::RawWindowHandle::AppKit(
                raw_window_handle::AppKitWindowHandle::new(
                    std::ptr::NonNull::new(self.ns_window as *mut _)
                        .ok_or(raw_window_handle::HandleError::Unavailable)?,
                ),
            );
            Ok(unsafe { raw_window_handle::WindowHandle::borrow_raw(raw) })
        }
    }

    impl raw_window_handle::HasDisplayHandle for HiddenWindow {
        fn display_handle(
            &self,
        ) -> Result<raw_window_handle::DisplayHandle<'_>, raw_window_handle::HandleError> {
            Ok(unsafe {
                raw_window_handle::DisplayHandle::borrow_raw(
                    raw_window_handle::RawDisplayHandle::AppKit(
                        raw_window_handle::AppKitDisplayHandle::new(),
                    ),
                )
            })
        }
    }

    // ── Public operations (called from dispatch_main closures) ──────────

    /// Create a new tab on the main thread.
    pub(super) fn create_tab(tabs_snapshot: &Arc<Mutex<Vec<TabInfo>>>) -> anyhow::Result<TabId> {
        with_state(|state| {
            let tab_id_str = format!("tab-{}", state.next_tab_id);
            state.next_tab_id += 1;

            let window = HiddenWindow::new()?;

            let tab_id_for_ipc = tab_id_str.clone();
            let webview = wry::WebViewBuilder::new()
                .with_url("about:blank")
                .with_ipc_handler(move |msg| {
                    let body = msg.body();
                    if let Some(result) = body.strip_prefix("__chatty_js_result:") {
                        handle_js_result(&tab_id_for_ipc, result.to_string());
                    }
                })
                .build(&window)
                .map_err(|e| anyhow::anyhow!("Failed to create WebView: {e}"))?;

            state.tabs.insert(
                tab_id_str.clone(),
                TabState {
                    window,
                    webview,
                    url: "about:blank".to_string(),
                    title: String::new(),
                },
            );

            update_snapshot(state, tabs_snapshot);
            Ok(TabId(tab_id_str))
        })
    }

    pub(super) fn close_tab(
        tab_id: &str,
        tabs_snapshot: &Arc<Mutex<Vec<TabInfo>>>,
    ) -> anyhow::Result<()> {
        with_state(|state| {
            if state.tabs.remove(tab_id).is_some() {
                state.js_callbacks.remove(tab_id);
                update_snapshot(state, tabs_snapshot);
                Ok(())
            } else {
                Err(anyhow::anyhow!("Tab {} not found", tab_id))
            }
        })
    }

    pub(super) fn navigate(
        tab_id: &str,
        url: &str,
        tabs_snapshot: &Arc<Mutex<Vec<TabInfo>>>,
    ) -> anyhow::Result<()> {
        with_state(|state| {
            if let Some(tab) = state.tabs.get_mut(tab_id) {
                tab.url = url.to_string();
                tab.webview
                    .load_url(url)
                    .map_err(|e| anyhow::anyhow!("Navigate failed: {e}"))?;
                update_snapshot(state, tabs_snapshot);
                Ok(())
            } else {
                Err(anyhow::anyhow!("Tab {} not found", tab_id))
            }
        })
    }

    pub(super) fn current_url(tab_id: &str) -> anyhow::Result<String> {
        with_state(|state| {
            if let Some(tab) = state.tabs.get(tab_id) {
                Ok(tab.url.clone())
            } else {
                Err(anyhow::anyhow!("Tab {} not found", tab_id))
            }
        })
    }

    pub(super) fn evaluate_js(
        tab_id: &str,
        script: &str,
        reply: oneshot::Sender<anyhow::Result<String>>,
    ) {
        with_state(|state| {
            if let Some(tab) = state.tabs.get(tab_id) {
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
                        state
                            .js_callbacks
                            .entry(tab_id.to_string())
                            .or_default()
                            .push(reply);
                    }
                    Err(e) => {
                        let _ = reply.send(Err(anyhow::anyhow!("evaluate_script failed: {e}")));
                    }
                }
            } else {
                let _ = reply.send(Err(anyhow::anyhow!("Tab {} not found", tab_id)));
            }
        });
    }

    pub(super) fn shutdown_all(tabs_snapshot: &Arc<Mutex<Vec<TabInfo>>>) {
        with_state(|state| {
            state.tabs.clear();
            state.js_callbacks.clear();
            update_snapshot(state, tabs_snapshot);
        });
    }

    // ── Internal helpers ────────────────────────────────────────────────

    fn handle_js_result(tab_id: &str, result: String) {
        with_state(|state| {
            if let Some(waiters) = state.js_callbacks.get_mut(tab_id) {
                if let Some(reply) = waiters.pop() {
                    if let Some(err_msg) = result.strip_prefix("\"__error:") {
                        let err_msg = err_msg.trim_end_matches('"');
                        let _ = reply.send(Err(anyhow::anyhow!("JS error: {err_msg}")));
                    } else {
                        let _ = reply.send(Ok(result));
                    }
                }
                if waiters.is_empty() {
                    state.js_callbacks.remove(tab_id);
                }
            }
        });
    }

    fn update_snapshot(state: &MainThreadState, snapshot: &Arc<Mutex<Vec<TabInfo>>>) {
        let infos: Vec<TabInfo> = state
            .tabs
            .iter()
            .map(|(id, tab)| TabInfo {
                id: TabId(id.clone()),
                url: tab.url.clone(),
                title: tab.title.clone(),
            })
            .collect();
        if let Ok(mut guard) = snapshot.lock() {
            *guard = infos;
        }
    }

    /// Verify the backend can create a WebView on the main thread.
    /// Called during WryBackend::new() to fail early if display is unavailable.
    pub(super) async fn verify_main_thread_webview() -> anyhow::Result<()> {
        dispatch_main(move || -> anyhow::Result<()> {
            let _window = HiddenWindow::new()?;
            // Initialize the thread-local state
            with_state(|_| {});
            Ok(())
        })
        .await
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// Linux / Windows: tao event loop on dedicated thread
// ══════════════════════════════════════════════════════════════════════════════

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

/// Wry-based browser backend.
///
/// **Linux / Windows**: Manages OS-native WebViews via wry+tao. The tao event
/// loop runs on a dedicated thread; all async methods communicate with it via
/// `EventLoopProxy` + oneshot channels.
///
/// **macOS**: Uses wry WebViews hosted in a hidden `NSWindow`, with all
/// operations dispatched to the main thread via GCD. This avoids conflicting
/// with GPUI's ownership of the main thread.
pub struct WryBackend {
    tabs_snapshot: std::sync::Arc<std::sync::Mutex<Vec<TabInfo>>>,

    /// Proxy to send commands to the event loop thread (Linux/Windows only).
    #[cfg(not(target_os = "macos"))]
    proxy: tao::event_loop::EventLoopProxy<inner::WryCommand>,
    #[cfg(not(target_os = "macos"))]
    _thread: Option<std::thread::JoinHandle<()>>,
}

/// Convenience impl for tests. Panics if the display server is unavailable.
/// Production code should use `WryBackend::new()` and handle the error.
impl Default for WryBackend {
    fn default() -> Self {
        // For macOS, new() is async. Use block_on in tests.
        #[cfg(target_os = "macos")]
        {
            let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
            rt.block_on(Self::new()).expect("WryBackend::new() failed")
        }
        #[cfg(not(target_os = "macos"))]
        Self::new_sync().expect("WryBackend::new() failed — display server required")
    }
}

impl WryBackend {
    /// Create a new wry backend (async version, required on macOS).
    ///
    /// # Platform notes
    ///
    /// - **macOS**: Dispatches to the main thread via GCD to create hidden
    ///   NSWindow + WKWebView. Works within GPUI's Cocoa run loop.
    /// - **Linux**: Spawns a background thread with tao + wry. Requires
    ///   WebKitGTK (`libwebkit2gtk-4.1-dev`).
    /// - **Windows**: Spawns a background thread with tao + wry. Requires
    ///   WebView2 (bundled with Windows 10+).
    pub async fn new() -> anyhow::Result<Self> {
        #[cfg(target_os = "macos")]
        {
            let tabs_snapshot = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));

            // Verify that we can create WebViews on the main thread.
            inner_macos::verify_main_thread_webview().await?;

            tracing::info!("WryBackend created — macOS native WebView (GCD dispatch)");

            Ok(Self { tabs_snapshot })
        }

        #[cfg(not(target_os = "macos"))]
        {
            Self::new_sync()
        }
    }

    /// Synchronous constructor (Linux/Windows only).
    #[cfg(not(target_os = "macos"))]
    fn new_sync() -> anyhow::Result<Self> {
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

#[async_trait]
impl BrowserBackend for WryBackend {
    async fn new_tab(&self) -> anyhow::Result<TabId> {
        #[cfg(target_os = "macos")]
        {
            let snap = self.tabs_snapshot.clone();
            inner_macos::dispatch_main(move || inner_macos::create_tab(&snap)).await
        }
        #[cfg(not(target_os = "macos"))]
        self.send(|reply| inner::WryCommand::NewTab { reply })
            .await?
    }

    async fn close_tab(&self, tab: &TabId) -> anyhow::Result<()> {
        #[cfg(target_os = "macos")]
        {
            let tab_id = tab.0.clone();
            let snap = self.tabs_snapshot.clone();
            inner_macos::dispatch_main(move || inner_macos::close_tab(&tab_id, &snap)).await
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
            let tab_id = tab.0.clone();
            let url = url.to_string();
            let snap = self.tabs_snapshot.clone();
            inner_macos::dispatch_main(move || inner_macos::navigate(&tab_id, &url, &snap)).await
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
            let tab_id = tab.0.clone();
            inner_macos::dispatch_main(move || inner_macos::current_url(&tab_id)).await
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
            let tab_id = tab.0.clone();
            let script = script.to_string();
            let (tx, rx) = tokio::sync::oneshot::channel();
            // Dispatch the script evaluation to the main thread; the result
            // comes back asynchronously via the IPC handler.
            inner_macos::dispatch_main(move || {
                inner_macos::evaluate_js(&tab_id, &script, tx);
            })
            .await;
            rx.await
                .map_err(|_| anyhow::anyhow!("JS eval reply channel dropped"))?
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
        anyhow::bail!("Screenshot is not supported by the wry backend")
    }

    async fn wait_for_load(&self, tab: &TabId, timeout_ms: u64) -> anyhow::Result<()> {
        #[cfg(target_os = "macos")]
        {
            let _ = tab;
            // On macOS, WKWebView navigation is asynchronous but doesn't expose
            // a page-load callback via the wry IPC path. Sleep for the requested
            // duration, letting the Cocoa run loop process navigation events.
            tokio::time::sleep(std::time::Duration::from_millis(timeout_ms)).await;
            Ok(())
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
        self.tabs_snapshot
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    async fn shutdown(&self) -> anyhow::Result<()> {
        #[cfg(target_os = "macos")]
        {
            let snap = self.tabs_snapshot.clone();
            inner_macos::dispatch_main(move || inner_macos::shutdown_all(&snap)).await;
            Ok(())
        }
        #[cfg(not(target_os = "macos"))]
        self.send(|reply| inner::WryCommand::Shutdown { reply })
            .await?
    }
}
