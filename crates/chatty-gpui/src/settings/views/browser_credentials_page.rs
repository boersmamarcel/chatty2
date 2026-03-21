use crate::settings::controllers::browser_credentials_controller;
use chatty_browser::credential::types::AuthMethod;
use chatty_browser::settings::BrowserCredentialsModel;
use gpui::{
    App, Context, Entity, FocusHandle, Focusable, FontWeight, Global, IntoElement, Render,
    SharedString, Styled, Window, div, prelude::*, px,
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
pub struct GlobalCredentialsTableView {
    pub view: Option<Entity<CredentialsTableView>>,
}

impl Global for GlobalCredentialsTableView {}

// ── Table view entity ───────────────────────────────────────────────────────

pub struct CredentialsTableView {
    focus_handle: FocusHandle,
}

impl CredentialsTableView {
    pub fn new(_window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus_handle = cx.focus_handle();
        Self { focus_handle }
    }

    fn show_add_credential_dialog(&self, window: &mut Window, cx: &mut Context<Self>) {
        let name_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("e.g. github, komoot"));
        let url_input = cx.new(|cx| InputState::new(window, cx).placeholder("https://example.com"));
        let username_selector_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("#email or input[name=email]"));
        let password_selector_input = cx
            .new(|cx| InputState::new(window, cx).placeholder("#password or input[type=password]"));
        let submit_selector_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("button[type=submit] (optional)"));
        let username_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("user@example.com"));
        let password_input = cx.new(|cx| InputState::new(window, cx).placeholder("password"));

        let view_entity = cx.entity().clone();

        window.open_dialog(cx, move |dialog, _, _| {
            dialog
                .title("Add Login Credential")
                .overlay(true)
                .keyboard(true)
                .close_button(true)
                .overlay_closable(true)
                .w(px(520.))
                .child(
                    div().id("add-credential-form").child(
                        v_flex()
                            .gap_3()
                            .p_4()
                            // Name
                            .child(
                                v_flex()
                                    .gap_1()
                                    .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).child("Credential Name"))
                                    .child(Input::new(&name_input))
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(gpui::hsla(0., 0., 0.5, 1.))
                                            .child("A short unique name (e.g. github, strava)"),
                                    ),
                            )
                            // URL pattern
                            .child(
                                v_flex()
                                    .gap_1()
                                    .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).child("URL Pattern"))
                                    .child(Input::new(&url_input))
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(gpui::hsla(0., 0., 0.5, 1.))
                                            .child("The login page URL that triggers authentication"),
                                    ),
                            )
                            // Form login selectors
                            .child(
                                v_flex()
                                    .gap_1()
                                    .child(
                                        div()
                                            .text_sm()
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .child("Form Login — CSS Selectors"),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(gpui::hsla(0., 0., 0.5, 1.))
                                            .child(
                                                "CSS selectors for the login form fields. \
                                                 Leave all empty for session-capture mode (cookies only).",
                                            ),
                                    )
                                    .child(
                                        v_flex()
                                            .gap_2()
                                            .pt_1()
                                            .child(
                                                v_flex()
                                                    .gap_1()
                                                    .child(div().text_xs().child("Username selector"))
                                                    .child(Input::new(&username_selector_input)),
                                            )
                                            .child(
                                                v_flex()
                                                    .gap_1()
                                                    .child(div().text_xs().child("Password selector"))
                                                    .child(Input::new(&password_selector_input)),
                                            )
                                            .child(
                                                v_flex()
                                                    .gap_1()
                                                    .child(div().text_xs().child("Submit selector"))
                                                    .child(Input::new(&submit_selector_input)),
                                            ),
                                    ),
                            )
                            // Form login credentials (username/password)
                            .child(
                                v_flex()
                                    .gap_1()
                                    .child(
                                        div()
                                            .text_sm()
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .child("Login Credentials (form login only)"),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(gpui::hsla(0., 0., 0.5, 1.))
                                            .child(
                                                "Stored securely in the OS keyring. \
                                                 Leave empty for session-capture mode.",
                                            ),
                                    )
                                    .child(
                                        v_flex()
                                            .gap_2()
                                            .pt_1()
                                            .child(
                                                v_flex()
                                                    .gap_1()
                                                    .child(div().text_xs().child("Username"))
                                                    .child(Input::new(&username_input)),
                                            )
                                            .child(
                                                v_flex()
                                                    .gap_1()
                                                    .child(div().text_xs().child("Password"))
                                                    .child(Input::new(&password_input)),
                                            ),
                                    ),
                            )
                            // Buttons
                            .child(
                                h_flex()
                                    .gap_2()
                                    .justify_end()
                                    .pt_4()
                                    .child(
                                        Button::new("cancel-credential")
                                            .label("Cancel")
                                            .on_click(move |_, window, cx| {
                                                window.close_dialog(cx);
                                            }),
                                    )
                                    .child(
                                        Button::new("save-credential")
                                            .primary()
                                            .label("Save")
                                            .on_click({
                                                let name_input = name_input.clone();
                                                let url_input = url_input.clone();
                                                let username_selector_input =
                                                    username_selector_input.clone();
                                                let password_selector_input =
                                                    password_selector_input.clone();
                                                let submit_selector_input =
                                                    submit_selector_input.clone();
                                                let username_input = username_input.clone();
                                                let password_input = password_input.clone();
                                                let view_entity = view_entity.clone();
                                                move |_, window, cx| {
                                                    let name = name_input
                                                        .read(cx)
                                                        .value()
                                                        .trim()
                                                        .to_string();
                                                    let url = url_input
                                                        .read(cx)
                                                        .value()
                                                        .trim()
                                                        .to_string();
                                                    let user_sel = username_selector_input
                                                        .read(cx)
                                                        .value()
                                                        .trim()
                                                        .to_string();
                                                    let pass_sel = password_selector_input
                                                        .read(cx)
                                                        .value()
                                                        .trim()
                                                        .to_string();
                                                    let submit_sel = submit_selector_input
                                                        .read(cx)
                                                        .value()
                                                        .trim()
                                                        .to_string();
                                                    let username = username_input
                                                        .read(cx)
                                                        .value()
                                                        .to_string();
                                                    let password = password_input
                                                        .read(cx)
                                                        .value()
                                                        .to_string();

                                                    if name.is_empty() {
                                                        window.push_notification(
                                                            "Credential name is required",
                                                            cx,
                                                        );
                                                        return;
                                                    }

                                                    if url.is_empty() {
                                                        window.push_notification(
                                                            "URL pattern is required",
                                                            cx,
                                                        );
                                                        return;
                                                    }

                                                    // Determine auth method based on selectors
                                                    let has_user_sel = !user_sel.is_empty();
                                                    let has_pass_sel = !pass_sel.is_empty();
                                                    let has_credentials =
                                                        !username.is_empty()
                                                            && !password.is_empty();

                                                    // If user provided credentials but no CSS
                                                    // selectors, auto-fill common defaults so it
                                                    // behaves as form-login instead of silently
                                                    // creating a session-capture profile.
                                                    let (user_sel, pass_sel, submit_sel) =
                                                        if !has_user_sel
                                                            && !has_pass_sel
                                                            && has_credentials
                                                        {
                                                            (
                                                                "input[type=\"email\"], input[name=\"email\"], input[name=\"username\"], #email, #username".to_string(),
                                                                "input[type=\"password\"]".to_string(),
                                                                "button[type=\"submit\"], input[type=\"submit\"]".to_string(),
                                                            )
                                                        } else {
                                                            (user_sel, pass_sel, submit_sel)
                                                        };

                                                    let has_user_sel = !user_sel.is_empty();
                                                    let has_pass_sel = !pass_sel.is_empty();
                                                    let is_form_login =
                                                        has_user_sel || has_pass_sel;

                                                    // Validate: if one selector provided, both are required
                                                    if is_form_login
                                                        && (!has_user_sel || !has_pass_sel)
                                                    {
                                                        window.push_notification(
                                                            "Both username and password selectors \
                                                             are required for form login",
                                                            cx,
                                                        );
                                                        return;
                                                    }

                                                    if is_form_login {
                                                        browser_credentials_controller::add_form_login(
                                                            name.clone(),
                                                            url,
                                                            user_sel,
                                                            pass_sel,
                                                            submit_sel,
                                                            cx,
                                                        );
                                                        // Store credentials in vault if provided
                                                        if has_credentials {
                                                            browser_credentials_controller::store_form_credentials(
                                                                name,
                                                                username,
                                                                password,
                                                                cx,
                                                            );
                                                        }
                                                    } else {
                                                        browser_credentials_controller::add_session_capture(
                                                            name, url, cx,
                                                        );
                                                    }

                                                    view_entity.update(cx, |_, cx| cx.notify());
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
                    .child("Auth Method"),
            )
            .child(
                div()
                    .w(px(50.))
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
        auth_method: AuthMethod,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let name_for_delete = name.clone();
        let view_for_delete = cx.entity().clone();
        let method_label = match auth_method {
            AuthMethod::SessionCapture => "Session Capture",
            AuthMethod::FormLogin => "Form Login",
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
                    .text_color(cx.theme().foreground)
                    .child(name),
            )
            .child(
                div()
                    .flex_1()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .overflow_hidden()
                    .child(url_pattern),
            )
            .child(
                div()
                    .w(px(120.))
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(method_label),
            )
            .child(
                h_flex().w(px(50.)).justify_end().child(
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
            .py_6()
            .gap_2()
            .items_center()
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child("No login credentials configured."),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child("Click \"+ Add Credential\" to add a website login."),
            )
    }
}

impl Focusable for CredentialsTableView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for CredentialsTableView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let entity = cx.entity().clone();
        let model = cx.global::<BrowserCredentialsModel>();
        let profiles = model.profiles.clone();

        let table = v_flex()
            .w_full()
            .rounded_md()
            .border_1()
            .border_color(cx.theme().border)
            .overflow_hidden()
            .child(self.render_header(cx))
            .map(|this| {
                if profiles.is_empty() {
                    this.child(self.render_empty(cx))
                } else {
                    this.children(profiles.iter().enumerate().map(|(ix, profile)| {
                        self.render_row(
                            ix,
                            profile.name.clone(),
                            profile.url_pattern.clone(),
                            profile.auth_method.clone(),
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

// ── Setting page entry point ────────────────────────────────────────────────

pub fn browser_credentials_page() -> SettingPage {
    SettingPage::new("Browser Credentials")
        .description(
            "Manage login credentials for websites the AI can authenticate to. \
             Secrets (passwords, session cookies) are stored in the OS keyring — \
             never written to disk or exposed to the AI.",
        )
        .resettable(false)
        .groups(vec![
            SettingGroup::new()
                .title("Login Profiles")
                .description(
                    "Each credential lets the AI authenticate to a website. \
                     Two auth methods are available:\n\n\
                     • Form Login — provide CSS selectors for the login form fields \
                     and a username/password. The AI fills the form automatically.\n\n\
                     • Session Capture — for OAuth/2FA sites, manually log in and \
                     capture session cookies. (Requires browser engine to be enabled.)",
                )
                .items(vec![SettingItem::render(|_options, window, cx| {
                    let view = if let Some(existing) = cx.try_global::<GlobalCredentialsTableView>()
                    {
                        if let Some(view) = existing.view.clone() {
                            view
                        } else {
                            let new_view = cx.new(|cx| CredentialsTableView::new(window, cx));
                            cx.set_global(GlobalCredentialsTableView {
                                view: Some(new_view.clone()),
                            });
                            new_view
                        }
                    } else {
                        let new_view = cx.new(|cx| CredentialsTableView::new(window, cx));
                        cx.set_global(GlobalCredentialsTableView {
                            view: Some(new_view.clone()),
                        });
                        new_view
                    };

                    div().w_full().child(view)
                })]),
        ])
}
