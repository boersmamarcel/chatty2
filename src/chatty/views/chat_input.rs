use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::ActiveTheme;
use gpui_component::button::Button;
use gpui_component::input::{Input, InputState};
use gpui_component::popover::Popover;
use std::sync::Arc;
use tracing::{debug, error, warn};

/// Callback type for sending messages
pub type SendMessageCallback = Arc<dyn Fn(String, &mut Context<ChatInputState>) + Send + Sync>;

/// Callback type for model selection changes
pub type ModelChangeCallback = Arc<dyn Fn(String, &mut Context<ChatInputState>) + Send + Sync>;

/// State for the chat input component
pub struct ChatInputState {
    pub input: Entity<InputState>,
    should_clear: bool,
    on_send: Option<SendMessageCallback>,
    on_model_change: Option<ModelChangeCallback>,
    selected_model_id: Option<String>,
    available_models: Vec<(String, String)>, // (id, display_name)
}

impl ChatInputState {
    pub fn new(input: Entity<InputState>) -> Self {
        Self {
            input,
            should_clear: false,
            on_send: None,
            on_model_change: None,
            selected_model_id: None,
            available_models: Vec::new(),
        }
    }

    /// Set the callback for sending messages
    pub fn set_on_send<F>(&mut self, callback: F)
    where
        F: Fn(String, &mut Context<ChatInputState>) + Send + Sync + 'static,
    {
        self.on_send = Some(Arc::new(callback));
    }

    /// Set the callback for model selection changes
    pub fn set_on_model_change<F>(&mut self, callback: F)
    where
        F: Fn(String, &mut Context<ChatInputState>) + Send + Sync + 'static,
    {
        self.on_model_change = Some(Arc::new(callback));
    }

    /// Set available models for selection
    pub fn set_available_models(
        &mut self,
        models: Vec<(String, String)>,
        default_id: Option<String>,
    ) {
        self.available_models = models;

        if self.selected_model_id.is_none() {
            self.selected_model_id =
                default_id.or_else(|| self.available_models.first().map(|(id, _)| id.clone()));
        }
    }

    /// Get the available models list
    pub fn available_models(&self) -> &[(String, String)] {
        &self.available_models
    }

    /// Set the selected model ID
    pub fn set_selected_model_id(&mut self, model_id: String) {
        self.selected_model_id = Some(model_id);
    }

    /// Send the current message
    pub fn send_message(&mut self, cx: &mut Context<Self>) {
        let message = self.input.read(cx).text().to_string();

        debug!(message = %message, "send_message called");

        if message.trim().is_empty() {
            warn!("Message is empty, not sending");
            return;
        }

        // Call the callback if set
        if let Some(on_send) = &self.on_send {
            debug!("on_send callback exists, calling it");
            on_send(message.clone(), cx);
        } else {
            error!("on_send callback is NOT set");
        }

        // Mark that we should clear on next render
        self.should_clear = true;
        debug!("Marked input for clearing");
    }

    /// Clear the input if needed
    pub fn clear_if_needed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.should_clear {
            self.input.update(cx, |input, cx| {
                input.set_value("", window, cx);
            });
            self.should_clear = false;
        }
    }

    /// Get display name for selected model
    pub fn get_selected_model_display_name(&self) -> String {
        self.selected_model_id
            .as_ref()
            .and_then(|id| {
                self.available_models
                    .iter()
                    .find(|(model_id, _)| model_id == id)
                    .map(|(_, name)| name.clone())
            })
            .unwrap_or_else(|| {
                if self.available_models.is_empty() {
                    "No models".to_string()
                } else {
                    "Select Model".to_string()
                }
            })
    }
}

/// Chat input component for rendering
#[derive(IntoElement)]
pub struct ChatInput {
    state: Entity<ChatInputState>,
}

impl ChatInput {
    pub fn new(state: Entity<ChatInputState>) -> Self {
        Self { state }
    }
}

impl RenderOnce for ChatInput {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let state_for_send = self.state.clone();
        let state_for_model = self.state.clone();
        let input_entity = self.state.read(cx).input.clone();

        // Model display name
        let model_display = self.state.read(cx).get_selected_model_display_name();
        let _no_models = self.state.read(cx).available_models.is_empty();

        // Model dropdown button
        let model_button = Button::new("model-select").label(model_display.clone());

        // Model popover
        let model_popover = Popover::new("model-menu")
            .trigger(model_button)
            .appearance(false)
            .content(move |_, _window, cx| {
                let state = state_for_model.clone();
                let models = state.read(cx).available_models.clone();
                let selected_id = state.read(cx).selected_model_id.clone();

                div()
                    .flex()
                    .flex_col()
                    .bg(cx.theme().background)
                    .border_1()
                    .border_color(cx.theme().border)
                    .rounded_md()
                    .shadow_md()
                    .p_1()
                    .min_w(px(200.0))
                    .when(models.is_empty(), |d| {
                        d.child(
                            div()
                                .px_3()
                                .py_2()
                                .text_sm()
                                .text_color(rgb(0x6b7280))
                                .child("No Models Available"),
                        )
                    })
                    .when(!models.is_empty(), |d| {
                        d.children(models.iter().map(|(id, name)| {
                            let id_clone = id.clone();
                            let state_for_click = state.clone();
                            let is_selected = selected_id.as_ref() == Some(id);

                            div()
                                .px_3()
                                .py_2()
                                .rounded_sm()
                                .cursor_pointer()
                                .when(is_selected, |d| d.bg(cx.theme().secondary))
                                .hover(|style| style.bg(cx.theme().secondary))
                                .text_sm()
                                .child(name.clone())
                                .on_mouse_down(MouseButton::Left, move |_event, _window, cx| {
                                    state_for_click.update(cx, |s, cx| {
                                        // Update selected model
                                        s.selected_model_id = Some(id_clone.clone());

                                        // Call the model change callback if set
                                        if let Some(on_change) = &s.on_model_change {
                                            on_change(id_clone.clone(), cx);
                                        }

                                        cx.notify();
                                    });
                                })
                        }))
                    })
            });

        div()
            .border_1()
            .px_3()
            .py_3()
            .rounded_2xl()
            .border_color(rgb(0xe5e7eb))
            .bg(cx.theme().secondary)
            .child(
                div()
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .child(Input::new(&input_entity).appearance(false)),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_2()
                            .child(div().flex_grow())
                            .child(model_popover)
                            .child(
                                // Send button
                                div()
                                    .px_3()
                                    .py_1()
                                    .rounded_sm()
                                    .bg(rgb(0xffa033))
                                    .text_color(rgb(0xffffff))
                                    .cursor_pointer()
                                    .hover(|style| style.bg(rgb(0xff8c1a)))
                                    .child("Send")
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        move |_event, _window, cx| {
                                            state_for_send.update(cx, |state, cx| {
                                                state.send_message(cx);
                                            });
                                        },
                                    ),
                            ),
                    ),
            )
    }
}
