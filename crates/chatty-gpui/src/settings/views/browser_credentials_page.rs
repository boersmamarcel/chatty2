use crate::settings::controllers::browser_credentials_controller;
use crate::settings::models::browser_credentials_store::{AuthType, BrowserCredentialsModel};
use gpui::{
    App, AsyncApp, Context, Entity, FocusHandle, Focusable, FontWeight, Global, IntoElement,
    Render, SharedString, Styled, Window, div, prelude::*, px,
};
use gpui_component::{
    ActiveTheme, Sizable, WindowExt as _,
    button::{Button, ButtonVariants},
    h_flex,
    input::{Input, InputState},
    setting::{SettingGroup, SettingItem, SettingPage},
    v_flex,
};
use gpui_component::{Icon, IconName};

// ── Global singleton ────────────────────────────────────────────────────────

#[derive(Default)]
pub struct GlobalBrowserCredentialsView {
    pub view: Option<Entity<BrowserCredentialsTableView>>,
}

impl Global for GlobalBrowserCredentialsView {}

// ── Table view entity ───────────────────────────────────────────────────────

pub struct BrowserCredentialsTableView {
    focus_handle: FocusHandle,
}

impl BrowserCredentialsTableView {
    pub fn new(_window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus_handle = cx.focus_handle();
        Self { focus_handle }
    }

    fn show_add_credential_dialog(&self, window: &mut Window, cx: &mut Context<Self>) {
        let name_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("e.g. komoot, strava"));
        let url_input = cx.new(|cx| InputState::new(window, cx).placeholder("e.g. komoot.com"));
        let view_entity = cx.entity().clone();

        window.open_dialog(cx, move |dialog, _, _| {
            dialog
                .title("Add Browser Credential")
                .overlay(true)
                .keyboard(true)
                .close_button(true)
                .overlay_closable(true)
                .w(px(500.))
                .child(
                    div().id("add-credential-form").child(
                        v_flex()
                            .gap_3()
                            .p_4()
                            .child(
                                v_flex()
                                    .gap_1()
                                    .child(div().text_sm().child("Credential Name"))
                                    .child(Input::new(&name_input))
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(gpui::hsla(0., 0., 0.5, 1.))
                                            .child(
                                                "A friendly name for this credential \
                                                 (used by the AI to identify it)",
                                            ),
                                    ),
                            )
                            .child(
                                v_flex()
                                    .gap_1()
                                    .child(div().text_sm().child("Website Domain"))
                                    .child(Input::new(&url_input))
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(gpui::hsla(0., 0., 0.5, 1.))
                                            .child(
                                                "The domain to capture cookies for \
                                                 (e.g. komoot.com)",
                                            ),
                                    ),
                            )
                            .child(
                                v_flex()
                                    .gap_2()
                                    .p_3()
                                    .rounded_md()
                                    .border_1()
                                    .border_color(gpui::hsla(210. / 360., 0.5, 0.8, 0.3))
                                    .bg(gpui::hsla(210. / 360., 0.5, 0.95, 0.3))
                                    .child(
                                        div()
                                            .text_sm()
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .child("How Session Capture Works"),
                                    )
                                    .child(div().text_xs().child(
                                        "1. A browser window will open to the website\n\
                                             2. Log in manually (handles 2FA, CAPTCHAs, etc.)\n\
                                             3. Click \"Capture Session\" when done\n\
                                             4. Cookies are stored for the AI to use later",
                                    )),
                            )
                            .child(
                                h_flex()
                                    .gap_2()
                                    .justify_end()
                                    .pt_4()
                                    .child(
                                        Button::new("cancel-credential").label("Cancel").on_click(
                                            move |_, window, cx| {
                                                window.close_dialog(cx);
                                            },
                                        ),
                                    )
                                    .child(
                                        Button::new("capture-credential")
                                            .primary()
                                            .label("Open Browser & Capture")
                                            .on_click({
                                                let name_input = name_input.clone();
                                                let url_input = url_input.clone();
                                                let view_entity = view_entity.clone();
                                                move |_, window, cx| {
                                                    let name = name_input
                                                        .read(cx)
                                                        .value()
                                                        .trim()
                                                        .to_string();
                                                    let domain = url_input
                                                        .read(cx)
                                                        .value()
                                                        .trim()
                                                        .to_string();

                                                    if name.is_empty() {
                                                        window.push_notification(
                                                            "Credential name is required",
                                                            cx,
                                                        );
                                                        return;
                                                    }

                                                    if domain.is_empty() {
                                                        window.push_notification(
                                                            "Website domain is required",
                                                            cx,
                                                        );
                                                        return;
                                                    }

                                                    // Clean up domain (remove protocol if provided)
                                                    let clean_domain = domain
                                                        .trim_start_matches("https://")
                                                        .trim_start_matches("http://")
                                                        .trim_end_matches('/')
                                                        .to_string();

                                                    // Start session capture in a background task
                                                    start_session_capture(
                                                        name.clone(),
                                                        clean_domain,
                                                        view_entity.clone(),
                                                        window,
                                                        cx,
                                                    );

                                                    window.close_dialog(cx);
                                                }
                                            }),
                                    ),
                            ),
                    ),
                )
        });
    }

    /// Render a header row for the credentials table.
    fn render_header(&self, cx: &Context<Self>) -> impl IntoElement {
        h_flex()
            .w_full()
            .px_3()
            .py_2()
            .border_b_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().muted)
            .child(
                div()
                    .w(px(120.))
                    .text_xs()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(cx.theme().muted_foreground)
                    .child("Name"),
            )
            .child(
                div()
                    .flex_1()
                    .text_xs()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(cx.theme().muted_foreground)
                    .child("URL Pattern"),
            )
            .child(
                div()
                    .w(px(120.))
                    .text_xs()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(cx.theme().muted_foreground)
                    .child("Cookies"),
            )
            .child(
                div()
                    .w(px(60.))
                    .text_xs()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(cx.theme().muted_foreground),
            )
    }

    /// Render a single credential row.
    fn render_row(
        &self,
        row_ix: usize,
        name: String,
        url_pattern: String,
        cookie_count: usize,
        captured_at: Option<String>,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let name_for_delete = name.clone();
        let view_for_delete = cx.entity().clone();

        let info_text = if let Some(at) = captured_at {
            // Show just the date portion
            let date = at.split('T').next().unwrap_or(&at);
            format!("{} cookies ({})", cookie_count, date)
        } else {
            format!("{} cookies", cookie_count)
        };

        h_flex()
            .w_full()
            .px_3()
            .py_2()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                div()
                    .w(px(120.))
                    .text_sm()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(cx.theme().foreground)
                    .child(name),
            )
            .child(
                div()
                    .flex_1()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(url_pattern),
            )
            .child(
                div()
                    .w(px(120.))
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(info_text),
            )
            .child(
                h_flex().w(px(60.)).justify_end().child(
                    Button::new(SharedString::from(format!("del-cred-{}", row_ix)))
                        .icon(Icon::new(IconName::Close))
                        .ghost()
                        .xsmall()
                        .on_click(move |_, _, cx| {
                            browser_credentials_controller::remove_credential(&name_for_delete, cx);
                            view_for_delete.update(cx, |_, cx| cx.notify());
                        }),
                ),
            )
    }

    /// Render the empty state.
    fn render_empty(&self, cx: &Context<Self>) -> impl IntoElement {
        v_flex()
            .w_full()
            .items_center()
            .py_6()
            .gap_2()
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child("No browser credentials configured."),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(
                        "Click \"Add Credential\" to capture a login session \
                         that the AI can use for authenticated browsing.",
                    ),
            )
    }
}

impl Focusable for BrowserCredentialsTableView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for BrowserCredentialsTableView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let entity = cx.entity().clone();
        let model = cx.global::<BrowserCredentialsModel>();
        let credentials = model.credentials.clone();

        let table = v_flex()
            .w_full()
            .rounded_md()
            .border_1()
            .border_color(cx.theme().border)
            .overflow_hidden()
            .child(self.render_header(cx))
            .map(|this| {
                if credentials.is_empty() {
                    this.child(self.render_empty(cx))
                } else {
                    this.children(credentials.iter().enumerate().map(|(ix, cred)| {
                        let (cookie_count, captured_at) = match &cred.auth_type {
                            AuthType::CapturedSession {
                                cookies,
                                captured_at,
                            } => (cookies.len(), Some(captured_at.clone())),
                        };
                        self.render_row(
                            ix,
                            cred.name.clone(),
                            cred.url_pattern.clone(),
                            cookie_count,
                            captured_at,
                            cx,
                        )
                        .into_any_element()
                    }))
                }
            });

        v_flex().size_full().gap_3().child(table).child(
            Button::new("add-credential-btn")
                .label("+ Add Credential")
                .primary()
                .on_click(move |_, window, cx| {
                    entity.update(cx, |view, cx| {
                        view.show_add_credential_dialog(window, cx);
                    });
                }),
        )
    }
}

/// Start a browser session capture flow.
///
/// Opens a visible Verso browser window to the target domain, lets the user
/// log in manually, then shows a notification with a "Capture" button to
/// extract cookies when the user is done.
fn start_session_capture(
    credential_name: String,
    domain: String,
    view_entity: Entity<BrowserCredentialsTableView>,
    window: &mut Window,
    cx: &mut App,
) {
    let url = format!("https://{}", domain);

    // Create a shared signal: the capture dialog will set this to true
    // when the user clicks "Capture Now".
    let capture_flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let cancel_flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));

    // Show a dialog that tells the user to log in, with a "Capture Now" button
    let capture_flag_for_dialog = capture_flag.clone();
    let cancel_flag_for_dialog = cancel_flag.clone();
    window.open_dialog(cx, move |dialog, _, _| {
        dialog
            .title("Session Capture In Progress")
            .overlay(true)
            .keyboard(true)
            .close_button(false)
            .overlay_closable(false)
            .w(px(450.))
            .child(
                div().id("capture-progress").child(
                    v_flex()
                        .gap_3()
                        .p_4()
                        .child(div().text_sm().child(
                            "A browser window is opening. Please log in to the website, \
                                     then click \"Capture Now\" when you're done.",
                        ))
                        .child(
                            div()
                                .text_xs()
                                .text_color(gpui::hsla(0., 0., 0.5, 1.))
                                .child(
                                    "Tip: Complete any 2FA or CAPTCHA challenges before capturing.",
                                ),
                        )
                        .child(
                            h_flex()
                                .gap_2()
                                .justify_end()
                                .pt_4()
                                .child(Button::new("cancel-capture").label("Cancel").on_click({
                                    let cancel_flag = cancel_flag_for_dialog.clone();
                                    move |_, window, cx| {
                                        cancel_flag
                                            .store(true, std::sync::atomic::Ordering::Relaxed);
                                        window.close_dialog(cx);
                                    }
                                }))
                                .child(
                                    Button::new("capture-now-btn")
                                        .primary()
                                        .label("Capture Now")
                                        .on_click({
                                            let capture_flag = capture_flag_for_dialog.clone();
                                            move |_, window, cx| {
                                                capture_flag.store(
                                                    true,
                                                    std::sync::atomic::Ordering::Relaxed,
                                                );
                                                window.close_dialog(cx);
                                            }
                                        }),
                                ),
                        ),
                ),
            )
    });

    // Spawn the async capture task
    cx.spawn(async move |cx: &mut AsyncApp| {
        let config = chatty_browser::BrowserEngineConfig {
            headless: false, // Visible window for manual login
            initial_url: Some(url.clone()),
            ..chatty_browser::BrowserEngineConfig::default()
        };

        let engine = chatty_browser::BrowserEngine::new(config);

        // Start the browser engine
        if let Err(e) = engine.start().await {
            tracing::warn!(
                error = %e,
                "Failed to start browser engine for session capture. \
                 Make sure Verso (versoview) is installed."
            );
            cx.update(|cx| {
                cx.refresh_windows();
            })
            .ok();
            return;
        }

        // Create a session and navigate to the target URL via DevTools
        let mut session = engine.create_session();
        if let Err(e) = session.navigate(&url).await {
            tracing::warn!(error = %e, "Failed to navigate for session capture");
            engine.stop().await;
            return;
        }

        // Poll until the user clicks "Capture Now" or "Cancel"
        loop {
            if cancel_flag.load(std::sync::atomic::Ordering::Relaxed) {
                tracing::info!("Session capture cancelled by user");
                engine.stop().await;
                return;
            }

            if capture_flag.load(std::sync::atomic::Ordering::Relaxed) {
                break;
            }

            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        }

        // Extract cookies from the session
        let cookies = match session.get_cookies().await {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(error = %e, "Failed to get cookies from session");
                engine.stop().await;
                return;
            }
        };

        let cookie_count = cookies.len();

        // Get the actual domain from the browser (may differ after redirects)
        let actual_domain = session
            .current_url_domain()
            .await
            .unwrap_or_else(|_| domain.clone());

        // Stop the browser
        engine.stop().await;

        // Store the credential
        cx.update(|cx| {
            browser_credentials_controller::store_captured_session(
                credential_name.clone(),
                actual_domain.clone(),
                cookies,
                cx,
            );
            tracing::info!(
                credential = %credential_name,
                domain = %actual_domain,
                cookies = cookie_count,
                "Session captured and stored"
            );
            // Refresh the table view
            view_entity.update(cx, |_, cx| cx.notify());
        })
        .ok();
    })
    .detach();
}

// ── Setting page entry point ────────────────────────────────────────────────

pub fn browser_credentials_page() -> SettingPage {
    SettingPage::new("Browser Credentials")
        .description(
            "Stored login sessions for authenticated web browsing. \
             Capture a session by logging in manually, then the AI can \
             use those cookies to access authenticated pages.",
        )
        .resettable(false)
        .groups(vec![
            SettingGroup::new()
                .title("Captured Sessions")
                .description(
                    "Each credential stores cookies from a manual login session. \
                 When the AI calls browser_auth with a credential name, those \
                 cookies are injected so it can access authenticated content.",
                )
                .items(vec![SettingItem::render(|_options, window, cx| {
                    let view = if let Some(existing) =
                        cx.try_global::<GlobalBrowserCredentialsView>()
                    {
                        if let Some(view) = existing.view.clone() {
                            view
                        } else {
                            let new_view =
                                cx.new(|cx| BrowserCredentialsTableView::new(window, cx));
                            cx.set_global(GlobalBrowserCredentialsView {
                                view: Some(new_view.clone()),
                            });
                            new_view
                        }
                    } else {
                        let new_view = cx.new(|cx| BrowserCredentialsTableView::new(window, cx));
                        cx.set_global(GlobalBrowserCredentialsView {
                            view: Some(new_view.clone()),
                        });
                        new_view
                    };

                    div().w_full().child(view)
                })]),
        ])
}
