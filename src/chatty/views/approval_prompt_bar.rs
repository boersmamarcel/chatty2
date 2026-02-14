use crate::assets::CustomIcon;
use gpui::{prelude::*, *};
use gpui_component::{ActiveTheme, Icon, Sizable, button::Button};
use std::sync::Arc;

pub type ApprovalCallback = Arc<dyn Fn(bool, &mut App) + Send + Sync>;
pub type ExpandCallback = Arc<dyn Fn(&mut App) + Send + Sync>;

#[derive(IntoElement)]
pub struct ApprovalPromptBar {
    command: String,
    is_sandboxed: bool,
    on_approve_deny: Option<ApprovalCallback>,
    on_expand: Option<ExpandCallback>,
}

impl ApprovalPromptBar {
    pub fn new(command: String, is_sandboxed: bool) -> Self {
        Self {
            command,
            is_sandboxed,
            on_approve_deny: None,
            on_expand: None,
        }
    }

    pub fn on_approve_deny<F>(mut self, callback: F) -> Self
    where
        F: Fn(bool, &mut App) + Send + Sync + 'static,
    {
        self.on_approve_deny = Some(Arc::new(callback));
        self
    }

    pub fn on_expand<F>(mut self, callback: F) -> Self
    where
        F: Fn(&mut App) + Send + Sync + 'static,
    {
        self.on_expand = Some(Arc::new(callback));
        self
    }

    fn sanitize_command(&self) -> String {
        // Remove actual newlines and escaped \n strings, truncate to max 100 chars
        let cleaned = self
            .command
            .replace('\n', " ")
            .replace('\r', " ")
            .replace("\\n", " ")
            .replace("\\r", " ");
        if cleaned.len() > 100 {
            format!("{}...", &cleaned[..97])
        } else {
            cleaned
        }
    }
}

impl RenderOnce for ApprovalPromptBar {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let warning_color = cx.theme().ring; // Used for errors/warnings
        let accent_color = cx.theme().accent;
        let bg_color = cx.theme().primary;
        let border_color = if self.is_sandboxed {
            accent_color
        } else {
            warning_color
        };

        // Platform-specific button labels
        #[cfg(target_os = "macos")]
        let (approve_label, deny_label, details_label) =
            ("Approve (⌘Y)", "Deny (⌘N)", "Details (⌘D)");
        #[cfg(not(target_os = "macos"))]
        let (approve_label, deny_label, details_label) =
            ("Approve (Ctrl+Y)", "Deny (Ctrl+N)", "Details (Ctrl+D)");

        // Clone callbacks for keyboard handler
        let approve_callback = self.on_approve_deny.clone();
        let deny_callback = self.on_approve_deny.clone();
        let details_callback = self.on_expand.clone();

        div()
            .w_full()
            .on_key_down(move |event: &KeyDownEvent, _window, cx| {
                use tracing::warn;

                let modifiers = event.keystroke.modifiers;
                let key = &event.keystroke.key;

                warn!(
                    "Approval bar key down: key={}, platform={}, control={}, alt={}, shift={}",
                    key, modifiers.platform, modifiers.control, modifiers.alt, modifiers.shift
                );

                // Use platform modifier (Cmd on macOS, Ctrl elsewhere)
                let cmd_or_ctrl = modifiers.platform;

                if cmd_or_ctrl {
                    warn!("Platform modifier pressed with key: {}", key);
                    match key.as_str() {
                        "y" => {
                            warn!("Approve shortcut triggered");
                            // Approve (Ctrl/Cmd + Y)
                            if let Some(ref cb) = approve_callback {
                                cb(true, cx);
                            }
                            cx.stop_propagation();
                        }
                        "n" => {
                            warn!("Deny shortcut triggered");
                            // Deny (Ctrl/Cmd + N)
                            if let Some(ref cb) = deny_callback {
                                cb(false, cx);
                            }
                            cx.stop_propagation();
                        }
                        "d" => {
                            warn!("Details shortcut triggered");
                            // Show Details (Ctrl/Cmd + D)
                            if let Some(ref cb) = details_callback {
                                cb(cx);
                            }
                            cx.stop_propagation();
                        }
                        _ => {
                            warn!("Unhandled key with platform modifier: {}", key);
                        }
                    }
                }
            })
            .px_3()
            .py_2()
            .bg(bg_color)
            .border_t_2()
            .border_color(border_color)
            .flex()
            .flex_row()
            .items_center()
            .gap_3()
            .overflow_hidden()
            .h(px(40.)) // Fixed height
            // Icon
            .child(
                Icon::new(if self.is_sandboxed {
                    CustomIcon::Lock
                } else {
                    CustomIcon::AlertCircle
                })
                .size_4()
                .text_color(cx.theme().foreground)
                .flex_shrink_0(),
            )
            // "Execute:" label
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().foreground)
                    .flex_shrink_0()
                    .child("Execute:"),
            )
            // Command text - single line with ellipsis
            .child(
                div()
                    .font_family("monospace")
                    .text_sm()
                    .text_color(cx.theme().foreground)
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .child(self.sanitize_command()),
            )
            // Badge
            .child(
                div()
                    .text_xs()
                    .px_2()
                    .py(px(1.))
                    .rounded_sm()
                    .flex_shrink_0()
                    .border_1()
                    .border_color(if self.is_sandboxed {
                        accent_color
                    } else {
                        warning_color
                    })
                    .text_color(if self.is_sandboxed {
                        accent_color
                    } else {
                        warning_color
                    })
                    .child(if self.is_sandboxed { "safe" } else { "unsafe" }),
            )
            // Buttons
            .child(
                div()
                    .flex()
                    .gap_2()
                    .flex_shrink_0()
                    .child(
                        Button::new("approve-floating")
                            .label(approve_label)
                            .small()
                            .on_click({
                                let callback = self.on_approve_deny.clone();
                                move |_event, _window, cx| {
                                    if let Some(ref cb) = callback {
                                        cb(true, cx);
                                    }
                                }
                            }),
                    )
                    .child(
                        Button::new("deny-floating")
                            .label(deny_label)
                            .small()
                            .on_click({
                                let callback = self.on_approve_deny.clone();
                                move |_event, _window, cx| {
                                    if let Some(ref cb) = callback {
                                        cb(false, cx);
                                    }
                                }
                            }),
                    )
                    .child(
                        Button::new("expand-trace")
                            .label(details_label)
                            .small()
                            .on_click({
                                let callback = self.on_expand.clone();
                                move |_event, _window, cx| {
                                    use tracing::warn;
                                    warn!("Details button clicked");
                                    if let Some(ref cb) = callback {
                                        warn!("Calling on_expand callback");
                                        cb(cx);
                                    } else {
                                        warn!("No on_expand callback set!");
                                    }
                                }
                            }),
                    ),
            )
    }
}
